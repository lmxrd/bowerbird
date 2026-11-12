//! `organizer-decrypt`: decrypt an encrypted folder by hand (see `tool.rs`
//! and docs/E2E_SPEC.md §10).

use organizer_e2e_crypto::tool::{self, Args, Secret, ToolError};
use std::io::BufRead;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // reqwest's TLS (rustls) with ring, as the server and the sync engine.
    let _ = rustls::crypto::ring::default_provider().install_default();
    match main_inner().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::from(2)
        }
    }
}

async fn main_inner() -> Result<std::process::ExitCode, ToolError> {
    let args = Args::parse(std::env::args().skip(1))?;
    let secret = if args.recovery {
        Secret::RecoveryKey(read_secret("ORGANIZER_RECOVERY_KEY", "Recovery key: ")?)
    } else {
        Secret::Passphrase(read_secret("ORGANIZER_PASSPHRASE", "Passphrase: ")?)
    };
    let report = tool::run(&args, &secret).await?;
    println!(
        "Decrypted {} files and {} folders into {}",
        report.files,
        report.folders,
        args.out.display()
    );
    for (id, why) in &report.failed {
        eprintln!("Left out {id}: {why}");
    }
    Ok(if report.failed.is_empty() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(1)
    })
}

/// From the variable, else a line of standard input (with a prompt on
/// standard error). The line is not hidden while typed: set the variable,
/// or pipe it in, where that matters.
fn read_secret(variable: &str, prompt: &str) -> Result<String, ToolError> {
    if let Ok(value) = std::env::var(variable) {
        return Ok(value);
    }
    eprint!("{prompt}");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim_end_matches(['\n', '\r']).to_string();
    if line.is_empty() {
        return Err(ToolError::Usage(format!(
            "Nothing given (or set {variable})"
        )));
    }
    Ok(line)
}
