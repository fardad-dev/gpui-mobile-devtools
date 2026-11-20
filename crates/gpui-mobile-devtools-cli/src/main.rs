//! `gpui-mobile-devtools`: opens the devtools viewer for a GPUI app running
//! on a phone.
//!
//! The app serves the viewer itself (port 9700); this tool only makes that
//! port reachable and opens the page:
//!
//! * No argument: finds the phone over adb, forwards port 9700 to this
//!   computer, and keeps the forward up while the phone reconnects.
//! * The viewer URL an app built with `lan` on logs
//!   (`http://192.168.1.23:9700/?token=…`): connects over Wi-Fi, no adb.
//!
//! Either way it waits for the app to answer, opens the viewer, and reports
//! when the app restarts.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::Duration;

/// The port the app listens on unless built with another.
const DEFAULT_PORT: u16 = 9700;

const HELP: &str = "\
gpui-mobile-devtools: Console and Network devtools for GPUI apps on phones

USAGE:
    gpui-mobile-devtools [OPTIONS] [CONNECTION]

ARGS:
    [CONNECTION]    The viewer URL the app logs when built with Wi-Fi access,
                    e.g. http://192.168.1.23:9700/?token=abc.
                    Omit it to connect over adb (USB or adb over Wi-Fi).

OPTIONS:
    -s, --serial <SERIAL>    adb device (default: $ANDROID_SERIAL, or the only device)
    -p, --port <PORT>        The app's devtools port (default: 9700)
        --no-open            Print the viewer URL instead of opening a browser
    -h, --help               Print help
    -V, --version            Print version

In the app (Cargo.toml: gpui-mobile-devtools = \"0.1\"):
    gpui_mobile_devtools::init(gpui_mobile_devtools::Config::new(\"my-app\"));
    let client = gpui_mobile_devtools::wrap_http_client(client);
";

struct Options {
    connection: Option<String>,
    serial: Option<String>,
    port: u16,
    open: bool,
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        connection: None,
        serial: std::env::var("ANDROID_SERIAL").ok().filter(|s| !s.is_empty()),
        port: DEFAULT_PORT,
        open: true,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("gpui-mobile-devtools {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-s" | "--serial" => options.serial = Some(value(&arg)?),
            "-p" | "--port" => options.port = parse_port(&value(&arg)?)?,
            "--no-open" => options.open = false,
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg} (see --help)")),
            _ if options.connection.is_none() => options.connection = Some(arg),
            _ => return Err(format!("unexpected argument {arg} (see --help)")),
        }
    }
    Ok(options)
}

fn parse_port(text: &str) -> Result<u16, String> {
    text.parse().map_err(|_| format!("not a port: {text}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    let target = match &options.connection {
        Some(connection) => Target::parse(connection)?,
        None => {
            let adb = Adb::find()?;
            let serial = adb.pick_device(options.serial.as_deref())?;
            adb.forward(&serial, options.port)?;
            println!("adb: forwarding tcp:{0} on this computer to tcp:{0} on {serial}", options.port);
            let target = Target {
                host: "127.0.0.1".into(),
                port: options.port,
                token: None,
            };
            let port = options.port;
            thread::spawn(move || adb.keep_forwarding(&serial, port));
            target
        }
    };

    let viewer = target.viewer_url();
    println!("viewer: {viewer}");
    println!("Leave this running; press Ctrl-C to stop.");
    watch_app(&target, options.open.then_some(viewer.as_str()));
    Ok(())
}

/// Where the app's devtools server is reachable from this computer.
struct Target {
    host: String,
    port: u16,
    token: Option<String>,
}

impl Target {
    /// Accepts `http://host:port/?token=…`, `ws://host:port/ws?token=…` or
    /// `host:port?token=…`.
    fn parse(connection: &str) -> Result<Self, String> {
        let rest = connection
            .split_once("://")
            .map_or(connection, |(_, rest)| rest);
        let (authority, query) = match rest.split_once('?') {
            Some((before, query)) => (before.split('/').next().unwrap_or(before), query),
            None => (rest.split('/').next().unwrap_or(rest), ""),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), parse_port(port)?),
            None => (authority.to_owned(), DEFAULT_PORT),
        };
        if host.is_empty() {
            return Err(format!("no host in {connection}"));
        }
        let token = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
            .map(str::to_owned);
        Ok(Self { host, port, token })
    }

    fn query(&self) -> String {
        self.token
            .as_ref()
            .map_or_else(String::new, |token| format!("?token={token}"))
    }

    /// The viewer page, served by the app.
    fn viewer_url(&self) -> String {
        format!("http://{}:{}/{}", self.host, self.port, self.query())
    }

    /// The app's `hello` event, if the app is reachable.
    fn hello(&self) -> Option<serde_json::Value> {
        let address = (self.host.as_str(), self.port).to_socket_addrs().ok()?.next()?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        write!(
            stream,
            "GET /hello{} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
            self.query(),
            self.host,
            self.port
        )
        .ok()?;
        let mut response = String::new();
        stream.read_to_string(&mut response).ok()?;
        let (head, body) = response.split_once("\r\n\r\n")?;
        if !head.starts_with("HTTP/1.1 200") {
            return None;
        }
        serde_json::from_str(body).ok()
    }
}

/// Prints when the app comes and goes, and opens `viewer` in a browser the
/// first time the app answers (the page lives in the app).
fn watch_app(target: &Target, mut viewer: Option<&str>) {
    let mut last_session: Option<String> = None;
    let mut waiting_said = false;
    loop {
        match target.hello() {
            Some(hello) => {
                let session = hello["session"].as_str().unwrap_or_default().to_owned();
                if last_session.as_deref() != Some(session.as_str()) {
                    println!(
                        "app: {} ({} {}, pid {}, devtools {})",
                        hello["app"].as_str().unwrap_or("?"),
                        hello["platform"].as_str().unwrap_or("?"),
                        hello["arch"].as_str().unwrap_or("?"),
                        hello["pid"],
                        hello["devtools"].as_str().unwrap_or("?"),
                    );
                    last_session = Some(session);
                    waiting_said = false;
                    if let Some(url) = viewer.take() {
                        open_browser(url);
                    }
                }
            }
            None => {
                if last_session.take().is_some() || !waiting_said {
                    println!(
                        "app: waiting for it at {}:{} (start the app, debug build)",
                        target.host, target.port
                    );
                    waiting_said = true;
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn open_browser(url: &str) {
    let opened = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).status()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).status()
    } else {
        Command::new("xdg-open").arg(url).status()
    };
    if !opened.is_ok_and(|status| status.success()) {
        println!("Open {url} in a browser.");
    }
}

struct Adb {
    path: PathBuf,
}

impl Adb {
    /// `$ADB`, `adb` on the `PATH`, or the SDK's platform-tools.
    fn find() -> Result<Self, String> {
        let exe = if cfg!(windows) { "adb.exe" } else { "adb" };
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(adb) = std::env::var("ADB") {
            candidates.push(adb.into());
        }
        candidates.push(exe.into());
        for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
            if let Ok(sdk) = std::env::var(var) {
                candidates.push(PathBuf::from(sdk).join("platform-tools").join(exe));
            }
        }
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            let home = PathBuf::from(home);
            candidates.push(home.join("Library/Android/sdk/platform-tools").join(exe));
            candidates.push(home.join("Android/Sdk/platform-tools").join(exe));
            candidates.push(home.join("AppData/Local/Android/Sdk/platform-tools").join(exe));
        }
        candidates
            .into_iter()
            .find(|path| {
                Command::new(path)
                    .arg("version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            })
            .map(|path| Self { path })
            .ok_or_else(|| {
                "adb not found. Install Android platform-tools, put adb on the PATH or set $ADB; \
                 or pass the connection string the app logs (Wi-Fi)."
                    .to_owned()
            })
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new(&self.path)
            .args(args)
            .output()
            .map_err(|error| format!("adb: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!(
                "adb {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    /// Serials of the devices that are online.
    fn devices(&self) -> Result<Vec<String>, String> {
        let list = self.run(&["devices"])?;
        Ok(list
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once('\t'))
            .filter(|(_, state)| state.trim() == "device")
            .map(|(serial, _)| serial.to_owned())
            .collect())
    }

    fn model(&self, serial: &str) -> String {
        self.run(&["-s", serial, "shell", "getprop", "ro.product.model"])
            .map(|model| model.trim().to_owned())
            .unwrap_or_default()
    }

    fn pick_device(&self, wanted: Option<&str>) -> Result<String, String> {
        let mut said_waiting = false;
        loop {
            let devices = self.devices()?;
            match (wanted, devices.as_slice()) {
                (Some(wanted), devices) if devices.iter().any(|serial| serial == wanted) => {
                    return Ok(wanted.to_owned());
                }
                (None, [only]) => {
                    println!("adb: using {only} ({})", self.model(only));
                    return Ok(only.clone());
                }
                (None, devices) if devices.len() > 1 => {
                    let list: Vec<String> = devices
                        .iter()
                        .map(|serial| format!("    {serial}  {}", self.model(serial)))
                        .collect();
                    return Err(format!(
                        "several devices are connected; pick one with -s <SERIAL> or $ANDROID_SERIAL:\n{}",
                        list.join("\n")
                    ));
                }
                _ => {
                    if !said_waiting {
                        match wanted {
                            Some(wanted) => println!("adb: waiting for {wanted}…"),
                            None => println!("adb: waiting for a device…"),
                        }
                        said_waiting = true;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    fn forward(&self, serial: &str, port: u16) -> Result<(), String> {
        let spec = format!("tcp:{port}");
        self.run(&["-s", serial, "forward", &spec, &spec]).map(drop)
    }

    /// A forward dies when the device disconnects; set it up again when the
    /// device comes back.
    fn keep_forwarding(&self, serial: &str, port: u16) {
        let mut online = true;
        loop {
            thread::sleep(Duration::from_secs(2));
            let now_online = self
                .run(&["-s", serial, "get-state"])
                .is_ok_and(|state| state.trim() == "device");
            if now_online && !online {
                match self.forward(serial, port) {
                    Ok(()) => println!("adb: {serial} is back, forward restored"),
                    Err(error) => println!("{error}"),
                }
            } else if !now_online && online {
                println!("adb: {serial} disconnected, waiting for it…");
            }
            online = now_online;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connection_strings() {
        let target = Target::parse("ws://192.168.1.23:9600/ws?token=abc").unwrap();
        assert_eq!((target.host.as_str(), target.port, target.token.as_deref()), ("192.168.1.23", 9600, Some("abc")));
        assert_eq!(target.viewer_url(), "http://192.168.1.23:9600/?token=abc");

        let target = Target::parse("http://10.0.0.5:9611/?token=x").unwrap();
        assert_eq!((target.host.as_str(), target.port, target.token.as_deref()), ("10.0.0.5", 9611, Some("x")));

        let target = Target::parse("10.0.0.5").unwrap();
        assert_eq!((target.port, target.token.as_deref()), (9700, None));
        assert_eq!(target.viewer_url(), "http://10.0.0.5:9700/");
        assert!(Target::parse("ws://:9600").is_err());
    }
}
