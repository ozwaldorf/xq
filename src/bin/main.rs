use std::{
    fs::File,
    io::{stdin, stdout, BufRead, IsTerminal, Read, Write},
    iter,
    path::PathBuf,
};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use clap_verbosity_flag::Verbosity;
use cli::input::Input;

use xq::{module_loader::PreludeLoader, run_query, InputError, Value};

use crate::cli::input::Tied;

mod cli;

#[derive(Parser, Debug)]
#[clap(author, about, version)]
#[clap(long_version(option_env!("LONG_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))))]
struct Cli {
    /// The query to run
    #[clap(default_value = ".")]
    query: String,

    /// Optional file to read. If input and output formats are unspecified,
    /// the file extension will be used to select the default.
    file: Option<PathBuf>,

    /// Read query from a file instead of arg
    #[clap(
        name = "file",
        short = 'f',
        long = "from-file",
        conflicts_with = "query",
        value_hint = clap::ValueHint::FilePath
    )]
    query_file: Option<PathBuf>,

    /// Enable json for both input and output
    #[arg(short = 'J', long, group = "format")]
    json: bool,

    /// Enable yaml for both input and output
    #[arg(short = 'Y', long, group = "format")]
    yaml: bool,

    /// Enable toml for both input and output
    #[arg(short = 'T', long, group = "format")]
    toml: bool,

    #[clap(flatten)]
    input_format: InputFormatArg,

    #[clap(flatten)]
    output_format: OutputFormatArg,

    #[clap(flatten)]
    verbosity: Verbosity,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default, clap::ValueEnum)]
enum SerializationFormat {
    #[default]
    Json,
    Yaml,
    Toml,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, clap::Args)]
struct InputFormatArg {
    /// Specify input format
    #[arg(
        long,
        value_enum,
        default_value_t,
        group = "input-format",
        conflicts_with = "format"
    )]
    input_format: SerializationFormat,

    /// Read input as json values
    #[arg(long, group = "input-format", conflicts_with = "format")]
    json_input: bool,

    /// Read input as yaml values
    #[arg(long, group = "input-format", conflicts_with = "format")]
    yaml_input: bool,

    #[arg(long, group = "input-format", conflicts_with = "format")]
    toml_input: bool,

    /// Treat each line of input will be supplied to the filter as a string.
    /// When used with --slurp, the whole input text will be supplied to the filter as a single
    /// string.
    #[arg(short = 'R', long, group = "input-format")]
    raw_input: bool,

    /// Single null is supplied to the program.
    /// The original input can still be read via input/0 and inputs/0.
    #[arg(short, long)]
    null_input: bool,

    /// Read input values into an array
    #[arg(short, long)]
    slurp: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, clap::Args)]
struct OutputFormatArg {
    /// Specify output format
    #[arg(
        long,
        value_enum,
        default_value_t,
        group = "output-format",
        conflicts_with = "format"
    )]
    output_format: SerializationFormat,

    /// Write output as json values
    #[arg(long, group = "output-format", conflicts_with = "format")]
    json_output: bool,

    /// Write output as yaml values
    #[arg(long, group = "output-format", conflicts_with = "format")]
    yaml_output: bool,

    /// Write output as yaml values
    #[arg(long, group = "output-format", conflicts_with = "format")]
    toml_output: bool,

    /// Output raw string if the output value was a string
    #[clap(short, long, conflicts_with = "output-format")]
    raw_output: bool,

    /// Compact output
    #[clap(short, long, conflicts_with = "output-format")]
    compact_output: bool,

    /// Colorize output where possible
    #[clap(short = 'C', long, default_value_t = true)]
    color: bool,
}

impl Cli {
    fn get_input_format(&self) -> SerializationFormat {
        if self.json || self.input_format.json_input {
            return SerializationFormat::Json;
        } else if self.yaml || self.input_format.yaml_input {
            return SerializationFormat::Yaml;
        } else if self.toml || self.input_format.toml_input {
            return SerializationFormat::Toml;
        } else {
            // If no options were specified, attempt to parse from the input file extension
            if let Some(path) = &self.file {
                if let Some(s) = path.extension().map(std::ffi::OsStr::to_string_lossy) {
                    match s.as_ref() {
                        "json" => return SerializationFormat::Json,
                        "yaml" => return SerializationFormat::Yaml,
                        "toml" => return SerializationFormat::Toml,
                        _ => {}
                    };
                }
            }

            self.input_format.input_format
        }
    }

    fn get_output_format(&self) -> SerializationFormat {
        if self.json || self.output_format.json_output {
            SerializationFormat::Json
        } else if self.yaml || self.output_format.yaml_output {
            SerializationFormat::Yaml
        } else if self.toml || self.output_format.toml_output {
            SerializationFormat::Toml
        } else {
            // If no options were specified, attempt to parse from the input file extension
            if let Some(path) = &self.file {
                if let Some(s) = path.extension().map(std::ffi::OsStr::to_string_lossy) {
                    match s.as_ref() {
                        "json" => return SerializationFormat::Json,
                        "yaml" => return SerializationFormat::Yaml,
                        "toml" => return SerializationFormat::Toml,
                        _ => {}
                    };
                }
            }

            self.output_format.output_format
        }
    }
}

fn init_log(verbosity: &Verbosity) -> Result<()> {
    use simplelog::{ColorChoice, CombinedLogger, Config, TermLogger, TerminalMode};
    let filter = match verbosity.log_level() {
        Some(l) => l.to_level_filter(),
        None => log::LevelFilter::Off,
    };
    CombinedLogger::init(vec![TermLogger::new(
        filter,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )])
    .with_context(|| "Unable to initialize logger")
}

fn print(should_color: bool, lang: &'static str, value: impl AsRef<[u8]>) -> Result<()> {
    let buf = value.as_ref();

    if should_color {
        bat::PrettyPrinter::new()
            .language(lang)
            .input_from_bytes(buf)
            .print()?;
    } else {
        stdout().write_all(buf)?;
    }

    Ok(())
}

fn run_with_input(cli: Cli, input: impl Input) -> Result<()> {
    let output_format = cli.get_output_format();

    let query = if let Some(path) = cli.query_file {
        log::trace!("Read query from file {path:?}");
        std::fs::read_to_string(path)?
    } else {
        log::trace!(
            "Read from query in arg (if it wasn't the default value): `{}`",
            cli.query
        );
        cli.query
    };
    let module_loader = PreludeLoader();

    let (context, input) = input.into_iterators();
    let result_iterator = run_query(&query, context, input, &module_loader)
        .map_err(|e| anyhow!("{:?}", e))
        .with_context(|| "compile query")?;

    let should_color = stdout().is_terminal() && cli.output_format.color;

    match output_format {
        SerializationFormat::Json => {
            for value in result_iterator {
                match value {
                    Ok(Value::String(s)) if cli.output_format.raw_output => {
                        println!("{s}\n");
                    }
                    Ok(value) => {
                        let mut value = if cli.output_format.compact_output {
                            serde_json::to_string::<Value>(&value)?
                        } else {
                            serde_json::to_string_pretty::<Value>(&value)?
                        };
                        value.push('\n');
                        print(should_color, "json", value)?;
                    }
                    Err(e) => eprintln!("Error: {e:?}"),
                }
            }
        }
        SerializationFormat::Yaml => {
            for value in result_iterator {
                match value {
                    Ok(value) => {
                        let mut buf = b"---\n".to_vec();
                        serde_yaml::to_writer(&mut buf, &value).context("Write to output")?;
                        buf.push(b'\n');
                        print(should_color, "yaml", buf)?;
                    }
                    Err(e) => eprintln!("Error: {e:?}"),
                }
            }
        }
        SerializationFormat::Toml => {
            for value in result_iterator {
                match value {
                    Ok(value) => {
                        if value.is_null() {
                            print(should_color, "toml", "\"null\"\n")?;
                            return Ok(());
                        }

                        if value.is_object() {
                            print(
                                should_color,
                                "toml",
                                if cli.output_format.compact_output {
                                    toml::to_string_pretty(&value)
                                } else {
                                    toml::to_string(&value)
                                }?,
                            )?;
                        } else {
                            let mut buf = String::new();
                            serde::Serialize::serialize(
                                &value,
                                toml::ser::ValueSerializer::new(&mut buf),
                            )
                            .context("Serialize value with toml")?;
                            buf.push('\n');
                            print(should_color, "toml", buf)?;
                        }
                    }
                    Err(e) => eprintln!("Error: {e:?}"),
                }
            }
        }
    }
    Ok(())
}

fn run_with_maybe_null_input(cli: Cli, input: impl Input) -> Result<()> {
    if cli.input_format.null_input {
        run_with_input(cli, input.null_input())
    } else {
        run_with_input(cli, input)
    }
}

fn run_with_maybe_slurp_null_input<I: Iterator<Item = Result<Value, InputError>>>(
    args: Cli,
    input: Tied<I>,
) -> Result<()> {
    if args.input_format.slurp {
        run_with_maybe_null_input(args, input.slurp())
    } else {
        run_with_maybe_null_input(args, input)
    }
}

fn read_and_run(cli: Cli, mut reader: impl Read + BufRead) -> Result<()> {
    if cli.input_format.raw_input {
        if cli.input_format.slurp {
            let mut input = String::new();
            reader.read_to_string(&mut input)?;
            run_with_maybe_null_input(cli, Tied::new(std::iter::once(Ok(Value::from(input)))))
        } else {
            let input = reader
                .lines()
                .map(|l| l.map(Value::from).map_err(InputError::new));
            run_with_maybe_null_input(cli, Tied::new(input))
        }
    } else {
        match cli.get_input_format() {
            SerializationFormat::Json => {
                let input = serde_json::de::Deserializer::from_reader(reader)
                    .into_iter::<Value>()
                    .map(|r| r.map_err(InputError::new));
                run_with_maybe_slurp_null_input(cli, Tied::new(input))
            }
            SerializationFormat::Yaml => {
                use serde::Deserialize;
                let input = serde_yaml::Deserializer::from_reader(reader)
                    .map(Value::deserialize)
                    .map(|r| r.map_err(InputError::new));
                run_with_maybe_slurp_null_input(cli, Tied::new(input))
            }
            SerializationFormat::Toml => {
                let mut buf = String::new();
                let input = reader
                    .lines()
                    // Ensure we always end with a delimiter
                    .chain(iter::once(Ok("+++".to_string())))
                    .filter_map(|res| {
                        match res {
                            Ok(line) => {
                                if line.trim() == "+++" {
                                    // Split on section dividers
                                    let value: Result<Value, _> =
                                        toml::from_str(&buf).map_err(InputError::new);
                                    buf.clear();
                                    Some(value)
                                } else {
                                    buf.push_str(&line);
                                    buf.push('\n');
                                    None
                                }
                            }
                            Err(e) => Some(Err(InputError::new(e))),
                        }
                    });
                run_with_maybe_slurp_null_input(cli, Tied::new(input))
            }
        }
    }
}

fn main() -> Result<()> {
    let cli: Cli = Cli::parse();
    init_log(&cli.verbosity)?;
    log::debug!("Parsed argument: {cli:?}");

    if let Some(path) = &cli.file {
        let file = std::io::BufReader::new(File::open(path)?);
        log::debug!("Opened file: {path:?}");
        read_and_run(cli, file)
    } else {
        read_and_run(cli, stdin().lock())
    }
}
