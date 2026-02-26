# 03_tokenized_ai_sandbox.md

## Architecture Review: Pillar 3 & 4 - Tokenized Agent Sandbox & Semantic Vibe

### Overview
This domain covers the safe integration of AI. The core philosophy is "AI as a zero-trust proposer." The Semantic Vibe Engine (ONNX/WASM) runs locally to gauge user intent, while the Tokenized Agent Sandbox ensures the LLM can only act via strictly defined, ephemeral tokens (`tool_req_xxx`) and that its actions are validated against the AST.

### Pros
*   **Safety First:** By treating the LLM as an untrusted user that can only *propose* `op_id`s, the system eliminates the risk of "rogue AI" directly mutating the database.
*   **Contextual Relevance:** The Semantic Vibe Engine reduces token cost and latency by only invoking the heavy LLM when the local classifier detects a specific intent or bottleneck.
*   **Privacy:** On-device classification keeps sensitive keystroke dynamics and mouse movements local; only high-level intent and relevant context are sent to the LLM.
*   **Auditability:** Every "thought" and proposal from the AI is logged and tied to a specific policy version, making AI behavior debugging possible.

### Cons & Risks
*   **Latency Overhead:** Even with "zero-trust," waiting for an LLM response adds significant latency compared to deterministic rules, potentially breaking flow if not handled asynchronously.
*   **Classifier Accuracy:** If the local ONNX model is inaccurate, it might trigger the LLM too often (cost/annoyance) or not enough (missed assistance).
*   **Token Management Complexity:** Managing the lifecycle of ephemeral `tool_req_xxx` tokens adds state management overhead and potential synchronization bugs.
*   **Prompt Injection:** Even with a sandbox, clever prompting might trick the LLM into proposing valid but "bad" operations that technically pass the AST but violate business intent in subtle ways.

### Single Points of Failure (SPOF)
*   **LLM Provider Availability:** Reliance on external APIs (OpenAI/Anthropic) means their downtime impacts AI features (must be mitigated by graceful degradation/feature flags).
*   **Vibe Model Drift:** If user behavior changes (e.g., new UI patterns), the static ONNX model might become obsolete and require a forced client update to regain accuracy.

## Testing Strategy

### Security Testing Matrix
We must treat the LLM interface as a hostile input vector, similar to a public API endpoint.

*   **Prompt Injection & Jailbreaking:** Use **Promptfoo** or **Garak** to systematically attack the system prompts. Attempt to trick the LLM into leaking system instructions, hallucinating non-existent tools, or generating toxic content.
*   **Fuzzing the Sandbox:** Generate random, malformed, and edge-case inputs for the `tool_req_xxx` tokens. Verify that the policy engine strictly enforces the `tool_req` validity and expires them correctly.
*   **Race Conditions:** Test the window between "Vibe detected" and "Token generation" vs "Action execution." Attempt to use a token *after* the AST state has changed to make it invalid.
*   **Adversarial AI:** Pit one LLM (Red Team) against the system LLM (Blue Team) to find logic gaps where a valid sequence of operations leads to an undesirable state.

### Specific Test Cases & Scenarios
*   **Token Expiry Race:** Generate a valid tool token, wait for *almost* the expiry time, and attempt to use it in parallel with a state change that invalidates the tool. Ensure the engine rejects it safely.
*   **Hallucinated Tool Calls:** Force the LLM to output calls to tools that don't exist or aren't currently unlocked. Verify the sandbox drops these silently or logs them as failures without crashing the application.
*   **Context Window Overflow:** Flood the "Vibe" vector with maximum-length context. Verify the system truncates or summarizes correctly before sending to the LLM to avoid token limit errors from the provider.
*   **Vibe Model Precision:** Run a dataset of recorded user sessions (mouse/keyboard events) through the WASM/ONNX model in a test harness. Measure precision/recall for intent detection against ground truth data.

### Tools
*   **Promptfoo:** For deterministic LLM eval and regression testing of prompts to ensure consistent behavior across model versions.
*   **Garak:** LLM vulnerability scanner (hallucination, data leakage, prompt injection).
*   **PyTorch/ONNX Runtime:** For offline validation and retraining of the Vibe classification model.
*   **Custom "Token Fuzzer":** A script to generate valid and invalid `tool_req` tokens at high volume to test the validator's throughput and correctness.
