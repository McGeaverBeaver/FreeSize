//! `freesize nextcloud`: what this machine keeps of its Nextcloud folders,
//! as the agent reports it to the Hub: each account the desktop client is
//! signed in to, its sync folders, and per folder how much is stored here
//! against how much is only a placeholder. For checking the Hub's "kept on
//! N devices" by hand.

use clap::Args;
use freesize_agent::nextcloud;
use freesize_core::format_size;

#[derive(Args, Debug)]
pub struct NextcloudArgs {
    /// Print the report as JSON, as it goes to the Hub.
    #[arg(long)]
    pub json: bool,
    /// List this many of the biggest files per folder (0 for none).
    #[arg(long, default_value_t = 10, value_name = "N")]
    pub files: usize,
}

pub fn main(args: NextcloudArgs) -> Result<(), String> {
    let accounts = nextcloud::desktop_accounts();
    if accounts.is_empty() {
        let files = nextcloud::config_files();
        return Err(if files.is_empty() {
            "no Nextcloud desktop client is set up on this machine (no nextcloud.cfg found)".into()
        } else {
            format!(
                "no accounts in {}",
                files
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    }
    let report = nextcloud::report(&accounts, freesize_agent::now());
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    for account in &report.accounts {
        println!("{} on {}", account.user, account.server);
        for folder in &account.folders {
            println!(
                "  {} = {}  ({} files, {}; {} on this machine, {})",
                folder.local,
                folder.remote,
                folder.files,
                format_size(folder.size),
                format_size(folder.on_disk),
                match folder.virtual_files.as_str() {
                    "off" | "" => "every file stored here".to_string(),
                    mode => format!(
                        "virtual files: {mode}, {} of {} files stored here",
                        folder.on_disk_files, folder.files
                    ),
                }
            );
            for dir in folder.dirs.iter().filter(|d| !d.path.contains('/')) {
                println!(
                    "    {:<40} {:>10}  {:>10} here",
                    dir.path,
                    format_size(dir.size),
                    format_size(dir.on_disk)
                );
            }
            for file in folder.big.iter().take(args.files) {
                println!(
                    "    {:<40} {:>10}  {}",
                    file.path,
                    format_size(file.size),
                    if file.on_disk { "here" } else { "online only" }
                );
            }
        }
    }
    Ok(())
}
