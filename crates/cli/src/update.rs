//! `freesize update`: is there a newer release, and install it over this
//! command line (the desktop app beside it too) once its signed sums check
//! out.

use clap::Args;
use freesize_agent::update::{self, Channel};
use freesize_agent::{UreqTransport, install};

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Install the newer release over this program, after checking its
    /// signature. Without this, only says whether there is one.
    #[arg(long)]
    pub install: bool,
    /// Pre-releases too.
    #[arg(long)]
    pub beta: bool,
}

pub fn main(args: UpdateArgs) -> Result<(), String> {
    let transport = UreqTransport::new();
    let channel = if args.beta {
        Channel::Beta
    } else {
        Channel::Stable
    };
    let Some(release) = update::check(&transport, channel, update::CURRENT)? else {
        println!("FreeSize {} is the newest release.", update::CURRENT);
        return Ok(());
    };
    println!(
        "FreeSize {} is out{}: {}",
        release.version,
        if release.prerelease {
            " (pre-release)"
        } else {
            ""
        },
        release.url
    );
    if !args.install {
        println!("Run `freesize update --install` to install it here.");
        return Ok(());
    }
    let name = update::asset_name().ok_or("no release is built for this platform")?;
    let sums = update::verified_sums(&transport, &release)?;
    println!("The release's sums are signed by the release key; downloading {name}…");
    let archive = update::download(&transport, &release, &sums, name)?;
    let files = install::unpack(
        &archive,
        name,
        &[
            "freesize",
            "freesize.exe",
            "freesize-gui",
            "freesize-gui.exe",
        ],
    )?;
    let exe = install::current_exe()?;
    let replaced = install::install_beside(&exe, &files)?;
    println!(
        "Installed FreeSize {} ({}); the previous files are kept as .old until the next run.",
        release.version,
        replaced.join(", ")
    );
    Ok(())
}
