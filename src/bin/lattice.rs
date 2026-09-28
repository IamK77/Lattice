//! Product entry: measure, interpret process arguments, dispatch, and return.
#[path = "lattice/cli.rs"]
mod cli;
#[path = "lattice/process_commands.rs"]
mod process_commands;
#[path = "lattice/startup.rs"]
mod startup;
#[path = "lattice/terminal_host.rs"]
mod terminal_host;

fn main() -> std::io::Result<()> {
    let startup = startup::Trace::start();
    let mode = std::env::args().nth(1);
    match cli::parse(mode, std::env::args_os().skip(2))? {
        cli::Command::Tui(resume) => terminal_host::run_tui(resume, startup),
        cli::Command::Recover(args) => process_commands::recover(args),
        cli::Command::Migrate(path) => process_commands::migrate(path),
        cli::Command::Verify(path) => process_commands::verify(path),
        cli::Command::Serve => process_commands::daemon(),
        cli::Command::Component(name) => process_commands::component(name),
        cli::Command::Prompt => process_commands::prompt(),
        cli::Command::Assembly => process_commands::assembly(),
        cli::Command::Compact(args) => process_commands::compact(args),
        cli::Command::Index => process_commands::index(),
        cli::Command::Export(args) => process_commands::export(args),
        cli::Command::Tidy => process_commands::tidy(),
        cli::Command::DebugFrame(args) => terminal_host::run_debug_frame(args),
        cli::Command::DebugTui(args) => terminal_host::run_debug_tui(args),
        cli::Command::Version => {
            println!("{}", lattice::VERSION);
            Ok(())
        }
        cli::Command::Help => {
            cli::print_help();
            Ok(())
        }
        cli::Command::Unknown(other) => {
            eprintln!("lattice: unknown command '{other}'\n");
            cli::print_help();
            std::process::exit(2);
        }
    }
}
