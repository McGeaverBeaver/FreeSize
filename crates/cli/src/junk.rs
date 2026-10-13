//! `freesize junk`: the junk cleaner on the command line. Lists what the
//! known places hold by category, ticked where it's safe, and with
//! `--clean` deletes what's ticked after showing it. "Check first" rules
//! are only cleaned when named with `--also`, and go to the quarantine on
//! their disk rather than being deleted.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use clap::Args;
use freesize_agent::junk as cleaner;
use freesize_agent::quarantine::{MountPlaces, Quarantine};
use freesize_core::format_size;
use freesize_core::junk::{self, Category, Finding, Safety};

#[derive(Args, Debug)]
pub struct JunkArgs {
    /// Delete what's ticked (safe and not in use), after listing it.
    /// Without this, nothing is touched.
    #[arg(long)]
    pub clean: bool,
    /// Also clean these rules, by the ids in the list. "Check first" rules
    /// go to the quarantine on their disk, where `freesize agent` and the
    /// Hub can put them back.
    #[arg(long = "also", value_name = "RULE")]
    pub also: Vec<String>,
    /// Only these rules.
    #[arg(long = "only", value_name = "RULE")]
    pub only: Vec<String>,
    /// Don't ask before cleaning.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Print the findings as JSON, with the paths of each.
    #[arg(long)]
    pub json: bool,
}

pub fn main(args: JunkArgs) -> Result<(), String> {
    let platform = junk::Platform::current();
    let env = junk::Env::system();
    let now = junk::now();
    let mut findings = junk::find(platform, &env, &junk::running_programs(), now);
    for id in args.only.iter().chain(&args.also) {
        if junk::rule_by_id(platform, id).is_none() {
            return Err(format!(
                "no rule \"{id}\" on this platform; `freesize junk` lists the ids"
            ));
        }
    }
    if !args.only.is_empty() {
        findings.retain(|f| args.only.contains(&f.rule.id.to_string()));
    }
    if args.json {
        return print_json(&findings, now);
    }
    print_list(&findings, &args.also);
    if !args.clean {
        println!(
            "Nothing was touched. `freesize junk --clean` deletes what's ticked; `--also RULE` \
             adds a rule (\"check first\" ones go to the quarantine)."
        );
        return Ok(());
    }
    let chosen: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.ticked() || args.also.contains(&f.rule.id.to_string()))
        .collect();
    if chosen.is_empty() {
        println!("Nothing to clean.");
        return Ok(());
    }
    for finding in &chosen {
        if finding.running {
            return Err(format!(
                "{} is running; close it first, or leave {} out",
                finding.rule.app, finding.rule.id
            ));
        }
        if finding.rule.safety == Safety::UseTool {
            return Err(format!(
                "{} is removed with its own tool: {}",
                finding.rule.title, finding.rule.tool
            ));
        }
    }
    let deleted: u64 = chosen
        .iter()
        .filter(|f| f.rule.safety == Safety::Safe)
        .map(|f| f.bytes)
        .sum();
    let aside: u64 = chosen
        .iter()
        .filter(|f| f.rule.safety == Safety::CheckFirst)
        .map(|f| f.bytes)
        .sum();
    let mut question = format!("Delete {} for good", format_size(deleted));
    if aside > 0 {
        question.push_str(&format!(
            " and move {} to the quarantine",
            format_size(aside)
        ));
    }
    if !args.yes && !ask(&format!("{question}?")) {
        return Err("nothing was touched".into());
    }
    let quarantine = Quarantine::new(Box::new(MountPlaces::new(&[])));
    let id = format!("j{now}");
    let mut outcome = cleaner::Outcome::default();
    let mut refills = 0;
    for finding in &chosen {
        let title = finding.rule.title;
        let move_aside = |paths: &[PathBuf]| -> Result<u64, String> {
            let moved = quarantine.move_in(&id, "freesize junk", title, paths, now);
            let bytes: u64 = moved.results.iter().flatten().sum();
            match moved.results.iter().find_map(|r| r.as_ref().err()) {
                Some(err) if bytes == 0 => Err(err.clone()),
                _ => Ok(bytes),
            }
        };
        match cleaner::clean(finding, &move_aside) {
            Ok(done) => {
                if finding.rule.refills {
                    refills += done.freed;
                }
                println!("  {title}: {}", done.detail());
                outcome.freed += done.freed;
                outcome.quarantined += done.quarantined;
                outcome.skipped.extend(done.skipped);
            }
            Err(err) => println!("  {title}: {err}"),
        }
    }
    println!();
    println!("Freed {}.", format_size(outcome.freed));
    if outcome.quarantined > 0 {
        println!(
            "Moved {} to the quarantine; `freesize agent` or the Hub can put it back or empty it.",
            format_size(outcome.quarantined)
        );
    }
    if !outcome.skipped.is_empty() {
        println!(
            "Skipped {} in use, for example {}.",
            match outcome.skipped.len() {
                1 => "1 file".to_string(),
                n => format!("{n} files"),
            },
            outcome.skipped[0].path.display()
        );
    }
    if refills > 0 {
        println!(
            "{} of that refills within days of normal use.",
            format_size(refills)
        );
    }
    Ok(())
}

fn print_list(findings: &[Finding], also: &[String]) {
    if findings.is_empty() {
        println!("No junk found in the known places.");
        return;
    }
    for category in Category::ALL {
        let in_category: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule.category == category)
            .collect();
        if in_category.is_empty() {
            continue;
        }
        println!("{}", category.title());
        for finding in in_category {
            let tick = if finding.ticked() || also.contains(&finding.rule.id.to_string()) {
                "[x]"
            } else {
                "[ ]"
            };
            println!(
                "  {tick} {:<34} {:>10}  {}",
                finding.rule.title,
                format_size(finding.bytes),
                finding.rule.id
            );
            let mut note = finding.rule.blurb.to_string();
            if finding.rule.refills {
                note.push_str(" Refills within days.");
            }
            println!("      {note}");
            let why = finding.why_not();
            if !why.is_empty() {
                println!("      {why}");
            }
        }
    }
    println!();
    println!(
        "Found {}; {} is safe to clean now.",
        format_size(junk::total(findings)),
        format_size(junk::safe_total(findings))
    );
}

fn print_json(findings: &[Finding], now: i64) -> Result<(), String> {
    let value = cleaner::report_json(findings, now);
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn ask(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!("freesize: {question} Not a terminal; pass --yes to go ahead.");
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    matches!(answer.trim(), "y" | "Y" | "yes")
}
