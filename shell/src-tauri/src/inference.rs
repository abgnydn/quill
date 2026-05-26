//! GGUF-backed rewrite engine. Wraps llama-cpp-2.
//!
//! Compiled only when the `llm` feature is on. Loads the model once at
//! startup and reuses the backend across rewrite calls. Each rewrite
//! builds a fresh context (cheap for small models) so concurrent calls
//! don't share KV cache state.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaLoraAdapter, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

/// LFM2.5 ChatML template. `<|startoftext|>` BOS is prepended by
/// `AddBos::Always` at tokenize time, so we don't include it here.
const PROMPT_TEMPLATE: &str =
    "<|im_start|>user\n{src}<|im_end|>\n<|im_start|>assistant\n";

/// Generation stop marker for LFM2.5 ChatML.
const STOP_MARKER: &str = "<|im_end|>";

/// Default editing instruction prepended to user text when no explicit
/// instruction is supplied.
const DEFAULT_INSTRUCTION: &str = "Fix the grammar and improve clarity:";

/// Send/Sync wrapper around the raw LoRA adapter handle.
///
/// `LlamaLoraAdapter` holds a `NonNull<llama_adapter_lora>` which the
/// upstream crate doesn't mark `Send` even though the underlying llama.cpp
/// adapter handle is process-local and thread-safe under external
/// synchronisation. We hold the inner adapter inside a `Mutex`, so all
/// mutating access is serialised — `unsafe impl Send + Sync` is sound for
/// our use.
struct AdapterCell(LlamaLoraAdapter);
unsafe impl Send for AdapterCell {}
unsafe impl Sync for AdapterCell {}

pub struct RewriteEngine {
    backend: LlamaBackend,
    model: LlamaModel,
    /// Optional personal LoRA adapter applied on top of the base model on
    /// every fresh context. `None` = base model only.
    adapter: Option<Mutex<AdapterCell>>,
    adapter_path: Option<PathBuf>,
    adapter_scale: f32,
    ctx_size: u32,
    max_new_tokens: i32,
}

impl RewriteEngine {
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self> {
        Self::load_with_adapter(model_path, None::<PathBuf>)
    }

    /// Load the base model and (optionally) attach a LoRA adapter. The
    /// adapter is loaded once at startup and re-applied to each new
    /// inference context.
    pub fn load_with_adapter<P: AsRef<Path>>(
        model_path: impl AsRef<Path>,
        adapter_path: Option<P>,
    ) -> Result<Self> {
        let backend = LlamaBackend::init().context("LlamaBackend::init")?;
        let model_params = pin!(LlamaModelParams::default());
        let model = LlamaModel::load_from_file(&backend, model_path.as_ref(), &model_params)
            .with_context(|| format!("loading GGUF at {}", model_path.as_ref().display()))?;

        let (adapter, adapter_path_owned) = match adapter_path {
            Some(p) => {
                let path: PathBuf = p.as_ref().to_path_buf();
                let ad = model
                    .lora_adapter_init(&path)
                    .with_context(|| format!("loading LoRA adapter at {}", path.display()))?;
                eprintln!("[quill] personal LoRA adapter loaded from {}", path.display());
                (Some(Mutex::new(AdapterCell(ad))), Some(path))
            }
            None => (None, None),
        };

        Ok(Self {
            backend,
            model,
            adapter,
            adapter_path: adapter_path_owned,
            adapter_scale: 1.0,
            ctx_size: 2048,
            max_new_tokens: 256,
        })
    }

    pub fn has_adapter(&self) -> bool {
        self.adapter.is_some()
    }

    pub fn adapter_path(&self) -> Option<&PathBuf> {
        self.adapter_path.as_ref()
    }

    /// Run a single-shot rewrite. Convenience wrapper that buffers tokens
    /// from `rewrite_streaming` into a single String. Use the streaming
    /// variant when you want per-token UI updates.
    pub fn rewrite(&self, text: &str, instruction: Option<&str>) -> Result<String> {
        self.rewrite_streaming(text, instruction, |_| {})
    }

    /// Streaming rewrite. `on_token` is invoked with each piece of newly
    /// generated text as it's decoded; the same accumulated text is also
    /// returned at the end (with `<|im_end|>` stripped). Callbacks are
    /// invoked from the calling thread, in order, synchronously.
    pub fn rewrite_streaming<F>(
        &self,
        text: &str,
        instruction: Option<&str>,
        mut on_token: F,
    ) -> Result<String>
    where
        F: FnMut(&str),
    {
        let src = format!("{} {}", instruction.unwrap_or(DEFAULT_INSTRUCTION), text);
        let prompt = PROMPT_TEMPLATE.replace("{src}", &src);

        let ctx_params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(self.ctx_size));
        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .context("creating llama context")?;

        if let Some(adapter_mu) = &self.adapter {
            let mut cell = adapter_mu
                .lock()
                .map_err(|_| anyhow::anyhow!("adapter mutex poisoned"))?;
            ctx.lora_adapter_set(&mut cell.0, self.adapter_scale)
                .map_err(|e| anyhow::anyhow!("lora_adapter_set: {e}"))?;
        }

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
            if piece.contains(STOP_MARKER) {
                break;
            }
            out.push_str(&piece);
            on_token(&piece);

            batch.clear();
            batch.add(token, n_cur, &[0], true)?;
            ctx.decode(&mut batch).context("decode step")?;
            n_cur += 1;
        }

        Ok(out.replace(STOP_MARKER, "").trim().to_string())
    }
}
