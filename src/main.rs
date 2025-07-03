use clap::Parser;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

mod git;
mod git2_rs; // Add new module
mod stats;
mod table;

/// Aggregate git blame stats across any git repository.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct RepoBlameArgs {
    /// Path to a git repository folder (specify a non-root folder if wanting to analyze a subfolder only).
    #[arg(short, long)]
    path: Option<std::path::PathBuf>,

    /// Include binary files in the blame stats (excluded by default)
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    include_binary: bool,

    /// Optional list of file extension(s) to exclude from the blame stats.
    /// Example: --exclude-by-extension md txt
    #[arg(short, long, num_args(1..))]
    exclude_by_extension: Option<Vec<String>>,

    /// Choose the git backend to use: 'cli' for git command line, 'git2' for libgit2.
    #[arg(long, value_parser = ["cli", "git2"], default_value = "cli")]
    git_backend: String,
}

fn main() {
    let args = RepoBlameArgs::parse();

    let binding = args.path.unwrap_or(PathBuf::from("."));
    let exclude_binary = !args.include_binary;

    let exclude_by_type = args.exclude_by_extension.unwrap_or_default();
    let repo_path = binding.as_path();
    let mut repo_stats = stats::RepoStats::new();

    // Choose backend based on args.git_backend
    if args.git_backend == "cli" {
        let mut git_tree = git::GitTree::new(repo_path);
        process_files(
            repo_path,
            exclude_binary,
            &exclude_by_type,
            &mut repo_stats,
            git_tree.iter().map(|p| (p.clone(), PathBuf::from(p))),
            |repo_p, file_p| {
                let blame = git::GitBlame::new(repo_p, file_p); // No mut, iter consumes
                Box::new(blame.iter())
            },
        );
    } else {
        // git2-rs backend
        match git2_rs::Git2Repository::new(repo_path) {
            Ok(git2_repo) => match git2_repo.tree_iter() {
                Ok(tree_iter) => {
                    process_files(
                        repo_path,
                        exclude_binary,
                        &exclude_by_type,
                        &mut repo_stats,
                        tree_iter.map(|p_str| {
                            let path_buf = PathBuf::from(&p_str);
                            (p_str, path_buf)
                        }),
                        |_, file_p| match git2_repo.blame_iter(file_p) {
                            Ok(blame_iter) => Box::new(blame_iter),
                            Err(e) => {
                                eprintln!(
                                    "\nError getting blame for file {} (git2): {}",
                                    file_p.display(),
                                     e // This 'e' is used
                                );
                                Box::new(std::iter::empty::<String>())
                            }
                        },
                    );
                }
                Err(_e) => { // Prefixed here
                    eprintln!("\nError iterating git tree (git2): {}", _e);
                }
            },
            Err(_e) => { // Prefixed here
                eprintln!("\nError opening repository (git2): {}", _e);
            }
        }
    }

    // Clear the line after processing all files
    print!("\r\x1B[2K");

    let sorted_authors = repo_stats.sorted_authors();
    let sorted_file_types_by_author = repo_stats.sorted_file_types_by_author();

    let table = table::TableDisplay::new(
        repo_path,
        &repo_stats,
        &sorted_authors,
        &sorted_file_types_by_author,
    );
    println!("{}", table);
}

fn process_files<'iter_life, F>( // Added lifetime parameter 'iter_life
    repo_path: &Path,
    exclude_binary: bool,
    exclude_by_type: &[String],
    repo_stats: &mut stats::RepoStats,
    file_iterator: impl Iterator<Item = (String, PathBuf)>,
    blame_provider: F,
) where
    F: Fn(&Path, &Path) -> Box<dyn Iterator<Item = String> + 'iter_life>, // Used 'iter_life
{
    file_iterator.for_each(|(display_path, file_path_for_blame)| {
        // Clear and print the current file being processed
        print!("\r\x1B[2K");
        print!("\r {}", display_path); // Use display_path for printing
        std::io::stdout().flush().unwrap();

        let file_path_obj = Path::new(&display_path); // Use display_path for extension and binary check
        let file_extension = file_path_obj.extension().and_then(|ext| ext.to_str());

        if exclude_binary {
            let mut full_path = PathBuf::from(repo_path);
            full_path.push(file_path_obj); // Use file_path_obj for binary check
            match is_binary_file(full_path.as_path()) {
                Ok(true) => return,
                Ok(false) => (),
                Err(_e) => { // Prefixed e here
                    // eprintln!("\nError checking if file is binary {}: {}", file_path_obj.display(), _e);
                    return; // Skip if error
                }
            }
        }

        if !exclude_by_type.is_empty()
            && exclude_by_type.contains(&file_extension.unwrap_or_default().to_string())
        {
            return;
        }

        // Use file_path_for_blame for the blame operation
        let blame_iter = blame_provider(repo_path, &file_path_for_blame);
        blame_iter
            .filter_map(|line| parse_email(&line))
            .for_each(|author_email| {
                repo_stats.increment_lines(&author_email, file_extension);
            });
    });
}

fn is_binary_file(file_path: &Path) -> Result<bool, std::io::Error> {
    let file = File::open(file_path)?;
    let mut buffer: Vec<u8> = vec![];
    file.take(1024_u64).read_to_end(&mut buffer)?;

    Ok(content_inspector::inspect(&buffer).is_binary())
}

fn parse_email(line: &str) -> Option<String> {
    const AUTHOR_MAIL_MARKER: &str = "author-mail "; // Note the space
    if line.starts_with(AUTHOR_MAIL_MARKER) {
        // Expects format "author-mail <email@domain.com>"
        let email_part = line[AUTHOR_MAIL_MARKER.len()..].trim();
        if email_part.starts_with('<') && email_part.ends_with('>') && email_part.len() > 2 {
            Some(email_part[1..email_part.len() - 1].to_string())
        } else {
            // Malformed, or perhaps an email without <> which we don't expect from our git2_rs output
            None
        }
    } else {
        None
    }
}
