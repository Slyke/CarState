use clap::{Parser, ValueEnum};
use std::{
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};
use styleguide_logger::{
    config::ConfigError,
    error_codes::{self, ErrorCodeMap},
};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Action {
    Add,
    Edit,
    #[value(alias = "rm")]
    Delete,
    Get,
    Search,
    #[value(alias = "list")]
    All,
    Validate,
}

#[derive(Parser)]
#[command(
    about = "Manage the styleguide logger's JSON5 error-code catalog",
    version
)]
struct Args {
    #[arg(long, short = 'a', value_enum)]
    action: Action,
    /// Catalog to read/write. Defaults to ERROR_FILE_PATH or the bundled errors.json5.
    #[arg(long, conflicts_with_all = ["error_input_file", "error_output_file"])]
    error_file: Option<PathBuf>,
    #[arg(long)]
    error_input_file: Option<PathBuf>,
    /// Destination for a mutation, or an explicit catalog export on a read action.
    #[arg(long)]
    error_output_file: Option<PathBuf>,
    #[arg(long, short = 'k')]
    error_key: Option<String>,
    #[arg(long)]
    new_error_key: Option<String>,
    #[arg(long)]
    error_code: Option<String>,
    #[arg(long, short = 'p', default_value = "")]
    prefix: String,
    #[arg(long)]
    deterministic: bool,
}

/// Cooperating writers lock before reading, so concurrent CLI mutations cannot lose updates.
struct CatalogLock(PathBuf);

impl CatalogLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let path = PathBuf::from(name);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self(path))
    }
}

impl Drop for CatalogLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<String, Box<dyn std::error::Error>> {
    if matches!(args.action, Action::Validate) {
        if let Some(code) = &args.error_code {
            return if error_codes::validate_error_code(&code.to_ascii_uppercase()) {
                Ok("Valid error code.".into())
            } else {
                Err("Invalid error code.".into())
            };
        }
    }
    if args.new_error_key.is_some() && !matches!(args.action, Action::Edit) {
        return Err("--new-error-key is only supported by edit".into());
    }
    if args.error_code.is_some() && !matches!(args.action, Action::Edit | Action::Validate) {
        return Err("--error-code is only supported by edit and validate".into());
    }
    let input = args
        .error_file
        .clone()
        .or(args.error_input_file.clone())
        .or_else(|| std::env::var_os("ERROR_FILE_PATH").map(PathBuf::from))
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("errors.json5"));
    let output = args.error_output_file.as_ref().unwrap_or(&input);
    let mutation = matches!(args.action, Action::Add | Action::Edit | Action::Delete);
    let writes = mutation || args.error_output_file.is_some();
    let _input_lock = if writes {
        Some(
            CatalogLock::acquire(&input)
                .map_err(|_| "Cannot lock catalog (another writer may be active)")?,
        )
    } else {
        None
    };
    let _output_lock = if writes && output != &input {
        Some(
            CatalogLock::acquire(output)
                .map_err(|_| "Cannot lock output catalog (another writer may be active)")?,
        )
    } else {
        None
    };
    let text = match std::fs::read_to_string(&input) {
        Ok(text) => text,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && matches!(args.action, Action::Add) =>
        {
            "{}".into()
        }
        Err(_) => return Err("Cannot read error-code file".into()),
    };
    let mut map =
        error_codes::parse_error_codes_with_environment(&text, &|name| std::env::var(name).ok())?;
    // Preserve reference strings instead of freezing resolved environment values on edits/exports.
    let mut raw: ErrorCodeMap =
        json5::from_str(&text).map_err(|_| "Invalid raw error-code catalog")?;
    let key = || {
        args.error_key
            .as_deref()
            .ok_or("--error-key is required for this action")
    };
    let message = match args.action {
        Action::Add => {
            let key = key()?;
            let code =
                error_codes::add_error_code(&mut map, key, &args.prefix, args.deterministic)?;
            raw.insert(key.to_owned(), code.clone());
            format!("Added error key: {key} - {code}")
        }
        Action::Edit => {
            let key = key()?;
            let code = error_codes::edit_error_code(
                &mut map,
                key,
                args.new_error_key.as_deref(),
                args.error_code.as_deref(),
            )?;
            let original = raw.remove(key).ok_or("Error key does not exist")?;
            let target = args.new_error_key.as_deref().unwrap_or(key);
            raw.insert(
                target.to_owned(),
                if args.error_code.is_some() {
                    code.clone()
                } else {
                    original
                },
            );
            format!("Edited error key: {target} - {code}")
        }
        Action::Delete => {
            let key = key()?;
            let code = error_codes::delete_error_code(&mut map, key)?;
            raw.remove(key);
            format!("Deleted error key: {key} - {code}")
        }
        Action::Get => map.get(key()?).cloned().ok_or("Error key not found")?,
        Action::Search => {
            if args.prefix.is_empty() {
                return Err("--prefix is required for search".into());
            }
            let prefix = args.prefix.to_ascii_uppercase();
            let matches: ErrorCodeMap = map
                .into_iter()
                .filter(|(_, code)| code.starts_with(&prefix))
                .collect();
            serde_json::to_string_pretty(&matches)?
        }
        Action::All => serde_json::to_string_pretty(&map)?,
        Action::Validate => "All error codes are valid.".into(),
    };
    if writes {
        save_raw_catalog(output, &raw)?;
    }
    Ok(message)
}

fn save_raw_catalog(path: &Path, raw: &ErrorCodeMap) -> Result<(), ConfigError> {
    error_codes::save_error_codes_preserving_references(path, raw)
}
