mod conversation;
mod inference;
mod repl;
mod routing_eval;
#[cfg(feature = "server")]
mod server;

use clap::{Parser, Subcommand};
use lh_core::{Config, Engine, Registry};
use lh_protocol::{
    RouteInput,
    json::{self, json},
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Local-first specialist routing")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Interactive line-oriented conversation with sticky routing.
    Chat {
        #[arg(long, default_value = "auto")]
        model: String,
    },
    #[cfg(feature = "server")]
    /// Serve the OpenAI-compatible HTTP subset.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8484")]
        bind: String,
    },
    /// Route a one-shot request and stream the specialist's answer.
    Run {
        #[arg(long, default_value = "auto")]
        model: String,
        #[arg(long)]
        image: Vec<String>,
        #[arg(long)]
        router: Option<String>,
        /// Transcribe by default, even when a prompt is supplied.
        #[arg(long, conflicts_with = "speak_out")]
        audio: Option<String>,
        /// Transcribe, then ask the default text specialist the prompt.
        #[arg(long, requires = "audio")]
        audio_question: bool,
        #[arg(long, conflicts_with = "image")]
        speak_out: Option<PathBuf>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        voice: Option<String>,
        #[arg(default_value = "")]
        prompt: String,
    },
    Models {
        #[command(subcommand)]
        command: Models,
    },
    Eval {
        #[command(subcommand)]
        command: Eval,
    },
    Plugins {
        #[command(subcommand)]
        command: Plugins,
    },
    Route {
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        image: Vec<String>,
        #[arg(long)]
        audio: Vec<String>,
        #[arg(long)]
        router: Option<String>,
        prompt: String,
    },
}
#[derive(Subcommand)]
enum Plugins {
    List,
}
#[derive(Subcommand)]
enum Models {
    /// Download and load a specialist or router model, then shut down.
    Pull { id: String },
}
#[derive(Subcommand)]
enum Eval {
    /// Evaluate labelled JSONL routing cases without running specialists.
    Routing {
        /// Disable all cascade shortcuts (explicit selection and modality safety remain).
        #[arg(long)]
        baseline: bool,
        #[arg(long, default_value = "evals/routing-tuning.jsonl")]
        file: PathBuf,
        #[arg(long)]
        router: Option<String>,
    },
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("lh: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let (config, directory) = Config::load(cli.config.as_deref())?;
    let registry = Registry::discover(&config, &directory)?;
    match cli.command {
        Commands::Chat { model } => {
            let mut engine = Engine::new(registry, config);
            let result = repl::run(&mut engine, model).await;
            engine.shutdown().await;
            result?;
        }
        #[cfg(feature = "server")]
        Commands::Serve { bind } => server::serve(Engine::new(registry, config), &bind).await?,
        Commands::Run {
            model,
            image,
            router,
            prompt,
            audio,
            audio_question,
            speak_out,
            language,
            voice,
        } => {
            let mut engine = Engine::new(registry, config);
            let result = inference::run(
                &mut engine,
                model,
                image,
                router.as_deref(),
                prompt,
                inference::AudioOptions {
                    audio,
                    audio_question,
                    speak_out,
                    language,
                    voice,
                },
            )
            .await;
            engine.shutdown().await;
            result?;
        }
        Commands::Models {
            command: Models::Pull { id },
        } => {
            let mut engine = Engine::new(registry, config);
            let result = inference::pull(&mut engine, &id).await;
            engine.shutdown().await;
            println!("{}", json::to_string_pretty(&result?)?);
        }
        Commands::Eval {
            command:
                Eval::Routing {
                    file,
                    router,
                    baseline,
                },
        } => {
            let mut engine = Engine::new(registry, config);
            if baseline {
                engine.config.cascade.enabled = false;
            }
            let result = routing_eval::evaluate(&mut engine, &file, router.as_deref()).await;
            engine.shutdown().await;
            let report = result?;
            println!("{}", json::to_string_pretty(&report)?);
            if report["error_count"].as_u64().unwrap_or(0) != 0 {
                return Err("routing evaluation encountered errors (see JSON report)".into());
            }
        }
        Commands::Plugins {
            command: Plugins::List,
        } => {
            let plugins: Vec<_> = registry.plugins.values().map(|entry| {
                let plugin = &entry.manifest.plugin;
                let enabled = config.plugins.get(&plugin.id).and_then(|c| c.enabled).unwrap_or(plugin.enabled);
                json!({"id":plugin.id,"version":plugin.version,"provides":plugin.provides,"capabilities":plugin.capabilities,"capabilities_source":"manifest","enabled":enabled,"manifest":entry.path,"status":if !enabled {"disabled"} else if entry.manifest.run.is_some() {"not-started"} else {"declarative"}})
            }).collect();
            println!(
                "{}",
                json::to_string_pretty(
                    &json!({"plugins":plugins,"specialists":registry.specialists.keys().collect::<Vec<_>>(),"routers":registry.routers.keys().collect::<Vec<_>>()})
                )?
            );
        }
        Commands::Route {
            explain,
            image,
            audio,
            router,
            prompt,
        } => {
            let mut engine = Engine::new(registry, config);
            let result = engine
                .route(
                    &RouteInput {
                        prompt,
                        images: image,
                        audio,
                        specialist: None,
                    },
                    router.as_deref(),
                )
                .await;
            engine.shutdown().await;
            let result = result?;
            println!(
                "{}",
                if explain {
                    json::to_string_pretty(&result)?
                } else {
                    json::to_string(&result)?
                }
            );
        }
    }
    Ok(())
}
