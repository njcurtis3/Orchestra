use orchestrad::daemon::{self, Command, USAGE};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match daemon::parse_args(&args) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("orchestrad: {why}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    match command {
        Command::Help => println!("{USAGE}"),
        Command::Version => println!("orchestrad {}", env!("CARGO_PKG_VERSION")),
        other => std::process::exit(dispatch(other)),
    }
}

#[cfg(target_os = "linux")]
fn dispatch(command: Command) -> i32 {
    match command {
        Command::Run { config, state_dir } => daemon::run(config, state_dir),
        other => daemon::client(other),
    }
}

#[cfg(not(target_os = "linux"))]
fn dispatch(_: Command) -> i32 {
    eprintln!("orchestrad runs on Linux");
    1
}
