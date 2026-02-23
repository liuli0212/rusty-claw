# OpenClaw 上下文与记忆管理机制深度解析

本文档基于 OpenClaw 源码，深入分析其对于 Agent 的 Context（上下文）及 Memory（记忆）的管理方案。在构建复杂的 Agent 系统时，记忆的“选取与组装”以及“存储与生命周期管理”是决定 Agent 智商下限的核心。

OpenClaw 采用了一种**双层架构（Dual-Layer Architecture）**，将短期会话上下文（Short-term Context）与长期知识库记忆（Long-term RAG Memory）物理分离，并在输入 LLM 之前通过精细的 Prompt 组装流水线将二者结合。

---

## 一、 整体架构概览

OpenClaw 的上下文系统分为三个核心层：

1.  **短期记忆 (Short-Term Memory / Transcripts)**: 负责单次会话中的多轮对话历史，以 JSONL 格式落盘，保证了即时的对话连贯性。
2.  **长期记忆 (Long-Term Memory / RAG)**: 负责跨会话的知识积累。基于 SQLite (`sqlite-vec` + `FTS5`) 实现向量与全文双路召回的混合搜索。
3.  **上下文组装管线 (Context Assembly Pipeline)**: 负责在每次调用 LLM 前，动态地将 System Prompt、工具列表、短期历史、长期记忆以及工作区状态组装、裁剪成最终的 Prompt。

---

## 二、 短期记忆 (Session History) 的管理

在解决“Memory 选读取和组装方案不好”的问题上，OpenClaw 对短期对话历史做了极其精细的控制。

### 1. 存储层：JSONL Transcript
会话历史并没有像简单的 Demo 那样放在内存的 Array 里，而是通过 `@mariozechner/pi-coding-agent` 的 `SessionManager` 落盘在 `~/.openclaw/sessions/<sessionId>.jsonl`。
每产生一轮对话，就会 Append 一行 JSON（包含 role, content, timestamp, token usage 等）。
**设计优势**：极低的写入开销，以及 Agent 崩溃重启后的完美恢复。

### 2. 注册表层：Session Store
文件 `src/config/sessions/store.ts` 维护了一个中央注册表 `sessions.json`。这个注册表保存了 `SessionEntry`（定义在 `src/config/sessions/types.ts`），记录了诸如：
*   `sessionId`: 唯一标识。
*   `inputTokens` / `outputTokens`: 用于监控上下文窗口消耗。
*   `compactionCount`: 当前会话被“压缩/总结”的次数。

### 3. 上下文组装与截断 (History Truncation & Sanitation)
仅仅读取 JSONL 扔给 LLM 是不够的（容易爆 Context）。OpenClaw 在 `src/agents/pi-embedded-runner/` 目录下实现了一条处理管线：

*   **回合限制 (`history.ts` - `limitHistoryTurns`)**: 根据配置，严格限制传入 LLM 的历史回合数，剔除过老的对话。
*   **工具输出截断 (`tool-result-truncation.ts`)**: 这是一个极其关键的设计。当某个工具（比如 `Bash` 或 `Read`）返回了超大文本（例如 5MB 的日志），如果在历史记录中保留它，后面的对话会立刻因为 Token 超限而崩溃。该模块使用启发式算法截断超长的工具输出，将其缩减为只保留头部或尾部的关键信息。
*   **Provider 数据清洗 (`google.ts` 等)**: 不同的模型对历史的要求不同。例如 Gemini 要求严格的 User/Model 交替，Anthropic 可能不支持某些特定的 Role 组合。在发往模型前，系统会进行格式清洗（Fixing turn ordering, dropping thinking blocks）。

---

## 三、 长期记忆 (Long-Term Memory / RAG) 的管理

OpenClaw 构建了一个自带的本地 RAG 系统，旨在解决“跨会话记忆”以及“知识沉淀”的问题。

### 1. 数据结构设计 (SQLite 驱动)
核心实现在 `src/memory/manager.ts` 和 `src/memory/memory-schema.ts`。OpenClaw 使用 SQLite 作为本地存储引擎：

*   **`files` 表**: 追踪被索引的原始文件（如 `MEMORY.md` 或历史 Session），记录 Hash 和修改时间，用于增量更新。
*   **`chunks` 表**: 将长文本切片（Chunking），记录来源、行号以及原始的 Embedding 数据。
*   **`embedding_cache` 表**: 嵌入向量的持久化缓存。调用 OpenAI、Gemini 等 API 生成 Embedding 很耗时且费钱，此表避免了对相同内容的重复计算。
*   **`chunks_vec` (虚拟表)**: 引入了 `sqlite-vec` 扩展，专门用于基于余弦相似度（Cosine Distance）的向量搜索。
*   **`chunks_fts` (虚拟表)**: 引入了 SQLite 原生的 `FTS5`（Full-Text Search），用于基于关键字的 BM25 检索。

### 2. 混合搜索策略 (Hybrid Search)
为了解决单纯向量搜索容易“找不准”具体方法名或代码变量名的问题，OpenClaw 在 `src/memory/manager-search.ts` 中采用了混合检索：

1.  **双路召回**: 同时触发 `chunks_vec`（向量相似度）和 `chunks_fts`（关键词匹配）。
2.  **动态权重 (Score Merging)**: 使用预设的 `vectorWeight` 和 `textWeight` 将两路得分进行归一化并加权求和。
3.  **查询扩展 (Query Expansion)**: 在执行 FTS 搜索前，会尝试从用户的自然语言 Query 中提取核心关键字，提升 BM25 的召回率。
4.  **时间衰减 (Temporal Decay)**: 对于记忆数据，越老的数据价值往往越低。系统可选地应用时间惩罚函数，优先呈现近期记忆。
5.  **最大边际相关性 (MMR)**: 为了避免召回的三段 Context 讲的都是同一件事，可选使用 MMR 算法增加召回结果的多样性。

---

## 四、 最终的 Prompt 组装逻辑 (Prompt Building)

解决上下文管理的最后一环，是如何将前面提取的短记忆、长记忆、以及系统指令干净地喂给模型。这部分逻辑集中在 `src/agents/pi-embedded-runner/system-prompt.ts`。

**System Prompt 的层次结构**：
1.  **Identity & Soul**: 最优先加载系统的人设和核心指导原则（例如从 `SOUL.md` 中读取）。
2.  **Runtime Info**: 注入当前的运行环境元数据（操作系统 macOS/Linux、Node 版本、当前调用的模型）。对于需要写代码或执行命令的 Agent，这些环境信息是极其关键的先验知识。
3.  **Tooling Context**: 动态注入当前启用的工具列表及其 Schema。OpenClaw 支持基于沙箱策略（Sandbox Policy）动态开启/关闭工具。
4.  **Skills Context**: 注入用户主动激活的特定“技能”（如 `test-driven-development`）。
5.  **Workspace Context**: 注入当前工作目录的特定约定文件（如 `AGENTS.md`, `TOOLS.md`）。
6.  **Retrieved Memory**: 如果触发了长期记忆搜索，将召回的 Top-K Chunk 组装到此部分。

### 上下文超载保护：压缩 (Compaction)
当上述组装的 Tokens 逼近模型的 Context Window 上限时，触发 `src/agents/pi-embedded-runner/compact.ts`。它会将旧的会话历史（Transcript）抛给一个较便宜的模型（如 `gpt-4o-mini` 或 `gemini-flash`）生成一份精简的 Summary，然后用这份 Summary 替换掉原始的数十轮旧对话。这确保了在极长对话下系统不会崩溃（OOM 或抛错）。

---

## 五、 总结与借鉴意义

如果你的 Agent 系统在上下文和记忆管理上遇到瓶颈，OpenClaw 的实现提供了以下几个极具价值的参考点：

1.  **不要将长短记忆混为一谈**：短记忆追求连贯和零延迟（JSONL 最佳），长记忆追求模糊匹配和泛化（SQLite Vec + FTS5 最佳）。
2.  **防御性上下文管理**：必须实现**工具输出截断 (Tool Result Truncation)**，这是单体 Agent 最容易暴毙的死穴（例如误 cat 了一个 10MB 的 bundle.js）。
3.  **混合检索是标配**：纯 Vector 检索在代码或精准指令场景下表现糟糕。必须结合全文检索（BM25），这是 RAG 的最佳实践。
4.  **动态 Prompt 装配**：不要写死 System Prompt。根据工具、环境、召回的记忆动态生成结构化的 System Prompt，能大幅提高模型遵循指令的概率。