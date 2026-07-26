// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Conductor Skills CLI - Validate, install, and manage Agent Skills
//!
//! # Usage
//!
//! ```bash
//! # Validate a skill directory
//! conductor-skills validate ./skills/conductor-midi-mapping
//!
//! # Validate all skills in a directory
//! conductor-skills validate ./skills
//!
//! # List installed skills
//! conductor-skills list
//!
//! # Install a skill
//! conductor-skills install ./my-skill
//! ```

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use clap::{Parser, Subcommand};
use colored::Colorize;
use conductor_daemon::skills::{
    SkillValidationError, get_skills_dir, install_skill, list_skills, validate_skill,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "conductor-skills")]
#[command(author, version, about = "Manage Conductor Agent Skills", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Validate a skill directory or all skills in a directory
    Validate {
        /// Path to skill directory or directory containing skills
        path: PathBuf,

        /// Show verbose output
        #[arg(short, long)]
        verbose: bool,
    },

    /// List installed skills
    List {
        /// Path to skills directory (defaults to ~/.conductor/skills)
        #[arg(short, long)]
        path: Option<PathBuf>,

        /// Show validation details
        #[arg(short, long)]
        verbose: bool,
    },

    /// Install a skill from a local directory
    Install {
        /// Path to skill directory to install
        source: PathBuf,

        /// Target skills directory (defaults to ~/.conductor/skills)
        #[arg(short, long)]
        target: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Validate { path, verbose } => cmd_validate(&path, verbose),
        Commands::List { path, verbose } => cmd_list(path, verbose),
        Commands::Install { source, target } => cmd_install(&source, target),
    }
}

fn cmd_validate(path: &PathBuf, verbose: bool) -> ExitCode {
    // Check if path is a skill directory or directory of skills
    if path.join("SKILL.md").exists() {
        // Single skill validation
        validate_single_skill(path, verbose)
    } else if path.is_dir() {
        // Directory of skills
        validate_skills_directory(path, verbose)
    } else {
        eprintln!("{} Path does not exist: {:?}", "Error:".red().bold(), path);
        ExitCode::FAILURE
    }
}

fn validate_single_skill(path: &PathBuf, verbose: bool) -> ExitCode {
    println!(
        "{} Validating skill at {:?}",
        "→".cyan(),
        path.file_name().unwrap_or_default()
    );

    match validate_skill(path) {
        Ok(skill) => {
            println!(
                "  {} {} (v{})",
                "✓".green().bold(),
                skill.metadata.name.green(),
                skill
                    .metadata
                    .metadata
                    .get("version")
                    .unwrap_or(&"?".to_string())
            );

            if verbose {
                println!("    Description: {}", skill.metadata.description);
                println!("    License: {}", skill.metadata.license);
                println!("    Body tokens: ~{}", skill.body_tokens);
                println!("    References: {}", skill.references.len());

                for ref_path in &skill.references {
                    println!("      - {:?}", ref_path.file_name().unwrap_or_default());
                }
            }

            ExitCode::SUCCESS
        }
        Err(errors) => {
            println!("  {} {} errors found", "✗".red().bold(), errors.len());

            for error in &errors {
                print_validation_error(error, verbose);
            }

            ExitCode::FAILURE
        }
    }
}

fn validate_skills_directory(path: &PathBuf, verbose: bool) -> ExitCode {
    println!("{} Validating skills in {:?}\n", "→".cyan(), path);

    let skills = list_skills(path);

    if skills.is_empty() {
        println!("{} No skills found in directory", "Warning:".yellow());
        return ExitCode::SUCCESS;
    }

    let mut valid_count = 0;
    let mut invalid_count = 0;

    for (skill_path, result) in &skills {
        let name = skill_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");

        match result {
            Ok(skill) => {
                valid_count += 1;
                println!(
                    "  {} {} (v{})",
                    "✓".green(),
                    name.green(),
                    skill
                        .metadata
                        .metadata
                        .get("version")
                        .unwrap_or(&"?".to_string())
                );

                if verbose {
                    println!("      {}", skill.metadata.description);
                }
            }
            Err(errors) => {
                invalid_count += 1;
                println!("  {} {}", "✗".red(), name.red());

                for error in errors {
                    print_validation_error(error, verbose);
                }
            }
        }
    }

    println!();
    println!(
        "{} {} valid, {} invalid",
        "Summary:".bold(),
        valid_count.to_string().green(),
        if invalid_count > 0 {
            invalid_count.to_string().red()
        } else {
            invalid_count.to_string().green()
        }
    );

    if invalid_count > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_validation_error(error: &SkillValidationError, verbose: bool) {
    match error {
        SkillValidationError::SkillMdNotFound(path) => {
            println!("      {} SKILL.md not found: {:?}", "•".red(), path);
        }
        SkillValidationError::InvalidFrontmatter(msg) => {
            println!("      {} Invalid frontmatter: {}", "•".red(), msg);
        }
        SkillValidationError::MissingRequiredField(field) => {
            println!("      {} Missing required field: {}", "•".red(), field);
        }
        SkillValidationError::NameMismatch { expected, actual } => {
            println!(
                "      {} Name mismatch: directory='{}', skill.name='{}'",
                "•".red(),
                expected,
                actual
            );
        }
        SkillValidationError::ReferenceNotFound(path) => {
            println!("      {} Reference not found: {:?}", "•".red(), path);
        }
        SkillValidationError::TokenLimitExceeded { actual, max } => {
            println!(
                "      {} Body exceeds token limit: {} > {} (reduce content or use references)",
                "•".red(),
                actual,
                max
            );
        }
        SkillValidationError::InvalidLicense(license) => {
            println!(
                "      {} Invalid license: {} (use SPDX identifier)",
                "•".red(),
                license
            );
            if verbose {
                println!("        Valid examples: MIT, Apache-2.0, GPL-3.0, BSD-3-Clause");
            }
        }
        SkillValidationError::IoError(msg) => {
            println!("      {} IO error: {}", "•".red(), msg);
        }
        SkillValidationError::InvalidToolPattern(pattern) => {
            println!("      {} Invalid tool pattern: {}", "•".red(), pattern);
            if verbose {
                println!("        Valid format: namespace:pattern (e.g., conductor:get_*)");
            }
        }
    }
}

fn cmd_list(path: Option<PathBuf>, verbose: bool) -> ExitCode {
    let skills_dir = path.or_else(get_skills_dir);

    let Some(skills_dir) = skills_dir else {
        eprintln!(
            "{} Could not determine skills directory",
            "Error:".red().bold()
        );
        return ExitCode::FAILURE;
    };

    if !skills_dir.exists() {
        println!(
            "{} No skills directory found at {:?}",
            "Info:".cyan(),
            skills_dir
        );
        println!("      Run 'conductor-skills install <path>' to install skills");
        return ExitCode::SUCCESS;
    }

    println!("{} Skills in {:?}\n", "→".cyan(), skills_dir);

    let skills = list_skills(&skills_dir);

    if skills.is_empty() {
        println!("  No skills installed");
        return ExitCode::SUCCESS;
    }

    for (skill_path, result) in &skills {
        let name = skill_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");

        match result {
            Ok(skill) => {
                let version = skill
                    .metadata
                    .metadata
                    .get("version")
                    .map(|s| s.as_str())
                    .unwrap_or("?");

                println!(
                    "  {} {} {}",
                    "•".cyan(),
                    name.bold(),
                    format!("v{}", version).dimmed()
                );

                if verbose {
                    println!("      {}", skill.metadata.description.dimmed());
                    println!(
                        "      License: {} | Tokens: ~{}",
                        skill.metadata.license, skill.body_tokens
                    );
                }
            }
            Err(errors) => {
                println!("  {} {} {}", "•".red(), name, "(invalid)".red());

                if verbose {
                    for error in errors {
                        println!("      {}", error.to_string().red());
                    }
                }
            }
        }
    }

    ExitCode::SUCCESS
}

fn cmd_install(source: &PathBuf, target: Option<PathBuf>) -> ExitCode {
    println!("{} Installing skill from {:?}", "→".cyan(), source);

    // Validate first
    match validate_skill(source) {
        Ok(skill) => {
            println!(
                "  {} Validated: {} (v{})",
                "✓".green(),
                skill.metadata.name,
                skill
                    .metadata
                    .metadata
                    .get("version")
                    .unwrap_or(&"?".to_string())
            );
        }
        Err(errors) => {
            println!("  {} Validation failed:", "✗".red().bold());
            for error in &errors {
                print_validation_error(error, true);
            }
            return ExitCode::FAILURE;
        }
    }

    // Install
    match install_skill(source, target) {
        Ok(installed_path) => {
            println!("  {} Installed to {:?}", "✓".green().bold(), installed_path);
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("  {} Installation failed: {}", "✗".red().bold(), e);
            ExitCode::FAILURE
        }
    }
}
