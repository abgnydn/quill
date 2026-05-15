//! GGUF-backed rewrite engine. Wraps llama-cpp-2.
//!
//! Compiled only when the `llm` feature is on. Loads the model once at
//! startup and reuses the backend across rewrite calls. Each rewrite
//! builds a fresh context (cheap for small models) so concurrent calls
//! don't share KV cache state.

use std::num::NonZeroU32;
use std::path::Path;
use std::pin::pin;

use anyhow::{Context, Result, bail};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

/// Gemma 3 chat template — matches the format used in `train/scripts/train.py`.
const PROMPT_TEMPLATE: &str =
    "<start_of_turn>user\n{src}<end_of_turn>\n<start_of_turn>model\n";

/// Default editing instruction prepended to user text when no explicit
/// instruction is supplied.
const DEFAULT_INSTRUCTION: &str = "Fix the grammar and improve clarity:";

pub struct RewriteEngine {
    backend: LlamaBackend,
    model: LlamaModel,
    ctx_size: u32,
    max_new_tokens: i32,
}

impl RewriteEngine {
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        let backend = LlamaBackend::init().context("LlamaBackend::init")?;
        let model_params = pin!(LlamaModelParams::default());
        let model = LlamaModel::load_from_file(&backend, model_path.as_ref(), &model_params)
            .with_context(|| format!("loading GGUF at {}", model_path.as_ref().display()))?;
        Ok(Self {
            backend,
            model,
            ctx_size: 2048,
            max_new_tokens: 256,
        })
    }

    /// Run a single-shot rewrite. `text` is the user content; `instruction`
    /// is an optional editing directive (e.g. "Paraphrase this sentence:").
    pub fn rewrite(&self, text: &str, instruction: Option<&str>) -> Result<String> {
        let src = format!("{} {}", instruction.unwrap_or(DEFAULT_INSTRUCTION), text);
        let prompt = PROMPT_TEMPLATE.replace("{src}", &src);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.ctx_size));
        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .context("creating llama context")?;

        let tokens = self
            .model
            .str_to_token(&prompt, AddBos::Always)
            .context("tokenizing prompt")?;

        let prompt_len = tokens.len() as i32;
        let n_len = prompt_len + self.max_new_tokens;
        if n_len > ctx.n_ctx() as i32 {
            bail!(
                "prompt + max_new_tokens ({n_len}) exceeds context size ({})",
                ctx.n_ctx()
            );
        }

        let mut batch = LlamaBatch::new(512.max(prompt_len as usize), 1);
        let last_idx = prompt_len - 1;
        for (i, tok) in tokens.into_iter().enumerate() {
            batch.add(tok, i as i32, &[0], i as i32 == last_idx)?;
        }
        ctx.decode(&mut batch).context("initial decode")?;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::dist(1337),
            LlamaSampler::greedy(),
        ]);

        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut out = String::new();
        let mut n_cur = batch.n_tokens();

        while n_cur <= n_len {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);

            if self.model.is_eog_token(token) {
                break;
            }
            let piece = self
                .model
                .token_to_piece(token, &mut decoder, true, None)
                .context("token_to_piece")?;

            // Gemma's <end_of_turn> is also our stop boundary even if the
            // model didn't emit a true EOG.
            if out.ends_with("<end_of_turn>") || piece.contains("<end_of_turn>") {
                break;
            }
            out.push_str(&piece);

            batch.clear();
            batch.add(token, n_cur, &[0], true)?;
            ctx.decode(&mut batch).context("decode step")?;
            n_cur += 1;
        }

        Ok(out
            .replace("<end_of_turn>", "")
            .trim()
            .to_string())
    }
}
