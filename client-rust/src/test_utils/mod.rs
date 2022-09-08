use std::{
    io,
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
};
use thiserror::Error;

use crate::{
    amman_config::AmmanConfig,
    blocking::AmmanClient,
    fs::write_amman_config,
    test_utils::consts::{VALIDATOR_PORT, VALIDATOR_RPC_PORT},
};

pub type AmmanProcessResult<T> = Result<T, AmmanProcessError>;

pub mod consts;
pub mod fs;

#[derive(Error, Debug)]
pub enum AmmanProcessError {
    #[error("amman was already started")]
    AmmanWasAlreadyStarted,

    #[error("amman already running on this machine with pid {0}, please kill it first and then continue")]
    AmmanAlreadyRunning(u32),

    #[error("amman is not running and thus cannot be killed")]
    AmmanCannotBeKilledIfNotRunning,

    #[error("failed to kill amman")]
    FailedToKillAmman(#[from] io::Error),
}

pub struct AmmanProcess {
    process: Option<Child>,
    pid: Option<u32>,
    client: AmmanClient,
    fixtures: PathBuf,
    assets_dir: PathBuf,
}

impl Clone for AmmanProcess {
    fn clone(&self) -> Self {
        // Cannot clone the process, thus this mainly serves to not have to query the pid
        // for an externally running amman again.
        // It is mainly used when attempting to restart the validator.
        Self {
            process: None,
            pid: self.pid.clone(),
            client: self.client.clone(),
            fixtures: self.fixtures.clone(),
            assets_dir: self.assets_dir.clone(),
        }
    }
}

impl AmmanProcess {
    pub fn new(client: AmmanClient) -> Self {
        let pid = pid_of_amman_running_on_machine(&client);
        let fixtures = std::fs::canonicalize(PathBuf::from("./tests/fixtures")).expect("fixtures");
        let assets_dir =
            std::fs::canonicalize(PathBuf::from("./tests/fixtures/assets")).expect("assets");
        Self {
            process: None,
            pid,
            client,
            fixtures,
            assets_dir,
        }
    }

    pub fn ensure_started(&mut self) -> AmmanProcessResult<()> {
        if self.process.is_some() {
            return Ok(());
        }
        if let Some(pid) = self.client.request_validator_pid().ok() {
            self.pid = Some(pid);
            return Ok(());
        }
        self.start()
    }

    pub fn start(&mut self) -> AmmanProcessResult<()> {
        self._start(None)?;
        Ok(())
    }

    fn _start(&mut self, amman_config: Option<&mut AmmanConfig>) -> AmmanProcessResult<()> {
        if self.process.is_some() {
            return Err(AmmanProcessError::AmmanWasAlreadyStarted);
        }
        if let Some(pid) = pid_of_amman_running_on_machine(&self.client) {
            return Err(AmmanProcessError::AmmanAlreadyRunning(pid));
        }

        let mut cmd = Command::new(consts::AMMAN_EXECUTABLE);
        cmd.current_dir(&self.fixtures);

        if std::env::var(consts::DUMP_AMMAN).is_err() {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        // we hold on to the config_file to ensure it doesn't get dropped before we started amman
        let (config_path, _config_file) = match amman_config {
            Some(config) => {
                if config.assets_folder.is_none() {
                    config.assets_folder = self.assets_dir.to_str().map(str::to_owned);
                }
                let (path, file) = write_amman_config(&config);
                (Some(path), Some(file))
            }
            None => (None, None),
        };
        cmd.arg("start");
        if let Some(config_path) = config_path {
            cmd.arg(config_path.to_str().unwrap());
        }
        eprintln!("Cmd: {:#?}", cmd);
        let process = cmd.spawn()?;

        eprint!("\nWaiting for pid");
        loop {
            match pid_of_amman_running_on_machine(&self.client) {
                Some(pid) => {
                    eprintln!(": {:#?}", pid);
                    break;
                }
                None => {}
            }
        }

        eprint!("Waiting for validator to be ready: ");
        wait_for_port(VALIDATOR_PORT);
        wait_for_port(VALIDATOR_RPC_PORT);
        eprint!("✔️\n");
        self.process = Some(process);

        Ok(())
    }

    pub fn restart(&mut self, amman_config: &mut AmmanConfig) -> AmmanProcessResult<()> {
        if self.started() {
            self.kill(true)?;
        }
        self._start(Some(amman_config))?;
        Ok(())
    }

    pub fn kill(&mut self, kill_external: bool) -> AmmanProcessResult<()> {
        if !self.started() {
            return Err(AmmanProcessError::AmmanCannotBeKilledIfNotRunning);
        }

        if let Some(process) = self.process.as_mut() {
            self.client
                .request_kill_amman()
                .expect("should kill amman properly");

            process.kill()?;
            process.wait()?;
            self.process = None;
        } else if let Some(pid) = self.pid {
            if kill_external {
                let mut process = Command::new(consts::AMMAN_EXECUTABLE).arg("stop").spawn()?;
                process.wait()?;
                eprintln!("Waiting for validator to shut down");
                wait_for_port_free(VALIDATOR_PORT);
                wait_for_port_free(VALIDATOR_RPC_PORT);
                self.pid = None;
            } else {
                eprintln!("Refusing to kill process that was not created by this runner ({:#?}). Please kill via `amman stop`",  pid);
            }
        }

        Ok(())
    }

    pub fn started(&self) -> bool {
        self.process.is_some() || self.pid.is_some()
    }
}

pub fn shutdown_amman() {
    let client = AmmanClient::new(None);

    if pid_of_amman_running_on_machine(&client).is_some() {
        client
            .request_kill_amman()
            .expect("failed to kill running amman");
        while pid_of_amman_running_on_machine(&client).is_some() {}
    }
}

pub fn pid_of_amman_running_on_machine(client: &AmmanClient) -> Option<u32> {
    match client.request_validator_pid() {
        Ok(pid) => Some(pid),
        Err(_) => None,
    }
}

fn scan_port(port: u16) -> bool {
    match TcpStream::connect(("0.0.0.0", port)) {
        Ok(_) => true,
        Err(_) => false,
    }
}

fn wait_for_port(port: u16) {
    while !scan_port(port) {}
}

fn wait_for_port_free(port: u16) {
    while scan_port(port) {}
}
