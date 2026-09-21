//! Native node lifecycle. Docker still owns the application and indexer.
use super::*;
use std::{
    cell::RefCell,
    fs::{File, OpenOptions, TryLockError},
    io::{Read, Seek, SeekFrom},
    net::TcpListener,
    path::Path,
    process::Child,
};

pub(super) struct InstanceLock(File);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Explicitly unlock: another thread may have forked a child that has not
        // reached exec (and closed inherited file descriptors) yet.
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(super) enum NodeSource {
    #[default]
    Docker,
    LocalBinary {
        binary: PathBuf,
        binary_version: String,
        config: PathBuf,
        log: PathBuf,
        process: Option<ProcessIdentity>,
    },
    ExternalRpc {
        rpc: String,
        config: PathBuf,
        p2p: String,
    },
}

impl NodeSource {
    pub(super) fn description(&self) -> String {
        match self {
            Self::Docker => "Docker".into(),
            Self::LocalBinary {
                binary,
                binary_version,
                ..
            } => {
                format!("{} ({binary_version})", binary.display())
            }
            Self::ExternalRpc { rpc, .. } => format!("external Zakura at {rpc}"),
        }
    }
}

/// Includes the unique per-run configuration path and OS start time, not just a PID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ProcessIdentity {
    pid: u32,
    identity: String,
}

impl ProcessIdentity {
    fn capture(pid: u32) -> Result<Self> {
        Ok(Self {
            pid,
            identity: process_identity(pid)?.context("Zakura exited during startup")?,
        })
    }

    fn is_running(&self) -> Result<bool> {
        Ok(process_identity(self.pid)?.as_deref() == Some(self.identity.as_str()))
    }

    pub(super) fn stop(&self) -> Result<()> {
        if !self.is_running()? {
            return Ok(());
        }
        signal(self.pid, "-TERM")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.is_running()? && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.is_running()? {
            signal(self.pid, "-KILL")?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.is_running()? && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        anyhow::ensure!(
            !self.is_running()?,
            "Zakura process {} did not stop; its data was retained",
            self.pid
        );
        Ok(())
    }
}

fn process_identity(pid: u32) -> Result<Option<String>> {
    let output = Command::new("ps")
        .args([
            "-ww",
            "-p",
            &pid.to_string(),
            "-o",
            "stat=",
            "-o",
            "lstart=",
            "-o",
            "args=",
        ])
        .env("LC_ALL", "C")
        .output()
        .context("checking native process identity")?;
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    if value.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        output.status.success(),
        "could not inspect native process {pid}"
    );
    // A zombie has exited, but its parent has not reaped it yet.
    let (state, identity) = value
        .split_once(char::is_whitespace)
        .context("invalid process identity")?;
    Ok((!state.starts_with('Z')).then(|| identity.trim().to_owned()))
}

fn signal(pid: u32, signal: &str) -> Result<()> {
    let output = Command::new("kill")
        .args([signal, &pid.to_string()])
        .output()?;
    if !output.status.success() && process_identity(pid)?.is_some() {
        bail!(
            "could not signal Zakura process {pid}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Networking {
    Desktop,
    Host,
}

impl Networking {
    fn discover() -> Result<Self> {
        let endpoint = match std::env::var("DOCKER_HOST") {
            Ok(value) if std::env::var_os("DOCKER_CONTEXT").is_none() => value,
            _ => docker_output([
                "context",
                "inspect",
                "--format",
                "{{.Endpoints.docker.Host}}",
            ])?,
        };
        anyhow::ensure!(
            endpoint.starts_with("unix://"),
            "local Zakura requires a local Docker daemon (remote Docker contexts are unsupported)"
        );
        let info: serde_json::Value =
            serde_json::from_str(&docker_output(["info", "--format", "{{json .}}"])?)?;
        let os = info["OperatingSystem"].as_str().unwrap_or_default();
        let rootless = info["SecurityOptions"].as_array().is_some_and(|options| {
            options
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s.contains("rootless")))
        });
        Self::from_environment(os, cfg!(target_os = "linux"), rootless)
    }

    fn from_environment(os: &str, linux: bool, rootless: bool) -> Result<Self> {
        anyhow::ensure!(
            !rootless,
            "local Zakura does not yet support rootless Docker networking"
        );
        if os.contains("Docker Desktop") || os.contains("OrbStack") {
            return Ok(Self::Desktop);
        }
        if linux {
            return Ok(Self::Host);
        }
        bail!(
            "local Zakura requires Docker Desktop, OrbStack, or native Docker Engine on Linux (found {os})"
        )
    }

    fn rpc_host(self) -> &'static str {
        match self {
            Self::Desktop => "host.docker.internal",
            Self::Host => "127.0.0.1",
        }
    }

    fn network(self, prefix: &str) -> String {
        match self {
            Self::Desktop => prefix.to_owned(),
            Self::Host => "host".into(),
        }
    }
}

pub(super) struct LocalHost {
    binary: PathBuf,
    version: String,
    networking: Networking,
    child: RefCell<Option<Child>>,
    log: RefCell<Option<PathBuf>>,
}

impl LocalHost {
    pub(super) fn new(path: &Path) -> Result<Self> {
        let binary = path
            .canonicalize()
            .with_context(|| format!("finding Zakura executable {}", path.display()))?;
        anyhow::ensure!(
            binary.is_file(),
            "Zakura executable must be a file: {}",
            binary.display()
        );
        let version = binary_version(&binary)?;
        let networking = Networking::discover()?;
        Ok(Self {
            binary,
            version,
            networking,
            child: RefCell::new(None),
            log: RefCell::new(None),
        })
    }

    fn check_child(&self) -> Result<()> {
        if let Some(child) = self.child.borrow_mut().as_mut()
            && let Some(status) = child.try_wait()?
        {
            let log = self
                .log
                .borrow()
                .as_ref()
                .map(|path| log_tail(path))
                .unwrap_or_default();
            bail!("local Zakura exited ({status}):\n{log}");
        }
        Ok(())
    }

    fn stop_child(&self) -> Result<()> {
        let mut child = self.child.borrow_mut();
        if let Some(process) = child.as_mut() {
            if process.try_wait()?.is_none() {
                signal(process.id(), "-TERM")?;
                let deadline = Instant::now() + Duration::from_secs(10);
                while process.try_wait()?.is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(100));
                }
                if process.try_wait()?.is_none() {
                    process.kill()?;
                }
            }
            process.wait()?;
        }
        *child = None;
        Ok(())
    }
}

impl StartHost for LocalHost {
    fn delete(&self, runtime: &Runtime, name: &InstanceName) -> Result<()> {
        // Stop RPC consumers before the node. Preserve metadata if termination fails.
        for service in ["app", "lightwalletd"] {
            let target = format!("{}-{service}", prefix(name));
            if container_exists(&target)? {
                docker(["rm", "-f", &target])?;
            }
        }
        self.stop_child()?;
        runtime.delete_instance_resources(name)
    }

    fn allocate(
        &self,
        runtime: &Runtime,
        name: &InstanceName,
        shutdown: &Shutdown,
    ) -> Result<Endpoints> {
        let prefix = prefix(name);
        let dir = runtime.instance_dir(name);
        fs::create_dir_all(&dir)?;
        let native = dir.join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&native)?;
        let native = native.canonicalize()?;
        let config = native.join("zakurad.toml");
        let log = native.join("zakura.log");
        *self.log.borrow_mut() = Some(log.clone());
        let mut instance = Instance {
            name: name.to_string(),
            version: 2,
            endpoints: Endpoints::default(),
            node: NodeSource::LocalBinary {
                binary: self.binary.clone(),
                binary_version: self.version.clone(),
                config: config.clone(),
                log: log.clone(),
                process: None,
            },
        };
        runtime.save_instance(name, &instance)?;
        if self.networking == Networking::Desktop {
            ensure_network(&prefix)?;
        }
        for suffix in ["wallet", "config", "lightwalletd"] {
            shutdown.check()?;
            ensure_volume(&format!("{prefix}-{suffix}"), name)?;
        }
        docker([
            "create",
            "--name",
            &format!("{prefix}-init"),
            "--label",
            &label(name),
            "-v",
            &format!("{prefix}-wallet:/data"),
            "-v",
            &format!("{prefix}-config:/config"),
            &app_image(),
            "init",
        ])?;
        shutdown.check()?;
        docker(["start", "-a", &format!("{prefix}-init")])?;
        shutdown.check()?;
        docker([
            "cp",
            &format!("{prefix}-init:/config/zakurad.toml"),
            config.to_str().context("invalid config path")?,
        ])?;
        let template = fs::read_to_string(&config)?;
        // Keep both reservations until all ports have been chosen to avoid duplicates.
        let rpc_socket = TcpListener::bind("127.0.0.1:0")?;
        let p2p_socket = TcpListener::bind("127.0.0.1:0")?;
        let rpc_port = rpc_socket.local_addr()?.port();
        let p2p_port = p2p_socket.local_addr()?.port();
        fs::write(
            &config,
            native_config(&template, &native.join("chain"), rpc_port, p2p_port)?,
        )?;
        drop((rpc_socket, p2p_socket));
        println!(
            "Using local Zakura: {} ({})\n  Config: {}",
            self.binary.display(),
            self.version,
            config.display()
        );
        let output = File::create(&log)?;
        let child = Command::new(&self.binary)
            .arg("--config")
            .arg(&config)
            .arg("start")
            .current_dir(&native)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .spawn()
            .context("starting local Zakura")?;
        *self.child.borrow_mut() = Some(child);
        let pid = self.child.borrow().as_ref().expect("just spawned").id();
        let identity = ProcessIdentity::capture(pid)?;
        if let NodeSource::LocalBinary { process, .. } = &mut instance.node {
            *process = Some(identity);
        }
        runtime.save_instance(name, &instance)?;
        let rpc = format!("http://127.0.0.1:{rpc_port}");
        wait_for_rpc(&rpc, shutdown, || self.check_child())?;
        let endpoints = start_companions(
            name,
            self.networking,
            rpc_port,
            &rpc,
            &format!("127.0.0.1:{p2p_port}"),
            "local_binary",
            shutdown,
        )?;
        instance.endpoints = endpoints.clone();
        runtime.save_instance(name, &instance)?;
        Ok(endpoints)
    }

    fn wait_ready(
        &self,
        endpoints: &Endpoints,
        app: &str,
        timeout: Duration,
        shutdown: &Shutdown,
    ) -> Result<()> {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            shutdown.check()?;
            self.check_child()?;
            if client
                .get(format!("{}/api/v1/health", endpoints.dashboard))
                .send()
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(());
            }
            if !container_running(app)? {
                bail!("app exited before becoming healthy:\n{}", docker_logs(app)?);
            }
            shutdown.wait_timeout(Duration::from_millis(250))?;
        }
        bail!(
            "dashboard did not become healthy within {} seconds:\n{}",
            timeout.as_secs(),
            docker_logs(app)?
        )
    }

    fn open_url(&self, url: &str) -> Result<()> {
        open_url(url)
    }

    fn wait_for_shutdown(&self, shutdown: &Shutdown) -> Result<()> {
        while !shutdown.try_interrupted() {
            self.check_child()?;
            if let Err(error) = shutdown.wait_timeout(Duration::from_millis(250))
                && !shutdown.try_interrupted()
            {
                return Err(error);
            }
        }
        Ok(())
    }
}

fn native_config(template: &str, chain: &Path, rpc_port: u16, p2p_port: u16) -> Result<String> {
    let mut config: toml::Value =
        toml::from_str(template).context("decoding generated Zakura configuration")?;
    config["rpc"]["listen_addr"] = toml::Value::String(format!("127.0.0.1:{rpc_port}"));
    config["network"]["listen_addr"] = toml::Value::String(format!("127.0.0.1:{p2p_port}"));
    config["state"]["cache_dir"] = toml::Value::String(
        chain
            .to_str()
            .context("chain directory is not UTF-8")?
            .into(),
    );
    Ok(toml::to_string_pretty(&config)?)
}

fn binary_version(binary: &Path) -> Result<String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "executing {} --version (check permissions and architecture)",
                binary.display()
            )
        })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            bail!("{} --version timed out", binary.display());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{} --version failed: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let version = String::from_utf8(output.stdout)?.trim().to_owned();
    anyhow::ensure!(
        !version.is_empty(),
        "{} --version returned no version",
        binary.display()
    );
    Ok(version)
}

fn wait_for_rpc(base: &str, shutdown: &Shutdown, check: impl Fn() -> Result<()>) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last_error = String::new();
    while Instant::now() < deadline {
        shutdown.check()?;
        check()?;
        match client.post(base).json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]})).send() {
            Ok(response) => {
                if response.status() == reqwest::StatusCode::UNAUTHORIZED { bail!("Zakura RPC requires authentication; use a dedicated local Regtest configuration with cookie auth disabled"); }
                let body: serde_json::Value = response.error_for_status()?.json()?;
                if let Some(chain) = body["result"]["chain"].as_str() {
                    anyhow::ensure!(chain.eq_ignore_ascii_case("regtest") || chain == "test", "expected Regtest, but Zakura reports {chain}");
                    let genesis: serde_json::Value = client.post(base).json(&serde_json::json!({"jsonrpc":"2.0","id":2,"method":"getblockhash","params":[0]})).send()?.error_for_status()?.json()?;
                    anyhow::ensure!(genesis["result"].as_str() == Some("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327"), "Zakura must use the standard Regtest genesis block");
                    return Ok(());
                }
                last_error = body.to_string();
            }
            Err(error) => last_error = error.to_string(),
        }
        shutdown.wait_timeout(Duration::from_millis(250))?;
    }
    bail!("Zakura RPC {base} was not ready within 120 seconds: {last_error}")
}

fn run_docker(args: Vec<String>) -> Result<()> {
    docker_inherit(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn start_companions(
    name: &InstanceName,
    networking: Networking,
    rpc_port: u16,
    public_rpc: &str,
    p2p: &str,
    node_mode: &str,
    shutdown: &Shutdown,
) -> Result<Endpoints> {
    let prefix = prefix(name);
    let network = networking.network(&prefix);
    let rpc_host = networking.rpc_host();
    let lwd_socket = TcpListener::bind("127.0.0.1:0")?;
    let app_socket = TcpListener::bind("127.0.0.1:0")?;
    let lwd_port = lwd_socket.local_addr()?.port();
    let app_port = app_socket.local_addr()?.port();
    let host = networking == Networking::Host;
    let mut lwd: Vec<String> = [
        "create",
        "--name",
        &format!("{prefix}-lightwalletd"),
        "--network",
        &network,
        "--label",
        &label(name),
        "--user",
        "0:0",
        "-v",
        &format!("{prefix}-lightwalletd:/var/lib/lightwalletd"),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if !host {
        lwd.extend([
            "--network-alias".into(),
            "lightwalletd".into(),
            "-p".into(),
            "127.0.0.1::9067".into(),
        ]);
    }
    lwd.extend([
        lightwalletd_image(),
        "--no-tls-very-insecure".into(),
        "--grpc-bind-addr".into(),
        if host {
            format!("127.0.0.1:{lwd_port}")
        } else {
            "0.0.0.0:9067".into()
        },
        "--rpchost".into(),
        rpc_host.into(),
        "--rpcport".into(),
        rpc_port.to_string(),
        "--rpcuser".into(),
        "unused".into(),
        "--rpcpassword".into(),
        "unused".into(),
        "--data-dir".into(),
        "/var/lib/lightwalletd".into(),
        "--log-file".into(),
        "/dev/stdout".into(),
    ]);
    run_docker(lwd)?;
    drop(lwd_socket);
    shutdown.check()?;
    docker(["start", &format!("{prefix}-lightwalletd")])?;
    let public_lwd = format!(
        "http://127.0.0.1:{}",
        if host {
            lwd_port
        } else {
            published_port(&format!("{prefix}-lightwalletd"), "9067/tcp")?
        }
    );
    let internal_lwd = if host {
        public_lwd.clone()
    } else {
        "http://lightwalletd:9067".into()
    };
    let mut app: Vec<String> = [
        "create",
        "--name",
        &format!("{prefix}-app"),
        "--network",
        &network,
        "--label",
        &label(name),
        "-v",
        &format!("{prefix}-wallet:/data"),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if !host {
        app.extend(["-p".into(), "127.0.0.1::8080".into()]);
    }
    for env in [
        format!(
            "TSZ_LISTEN={}",
            if host {
                format!("127.0.0.1:{app_port}")
            } else {
                "0.0.0.0:8080".into()
            }
        ),
        format!("TSZ_ZAKURA_RPC=http://{rpc_host}:{rpc_port}"),
        format!("TSZ_LIGHTWALLETD={internal_lwd}"),
        format!("TSZ_INSTANCE={name}"),
        format!("TSZ_PUBLIC_ZAKURA_RPC={public_rpc}"),
        format!("TSZ_PUBLIC_LIGHTWALLETD={public_lwd}"),
        format!("TSZ_PUBLIC_P2P={p2p}"),
        format!("TSZ_NODE_MODE={node_mode}"),
    ] {
        app.extend(["-e".into(), env]);
    }
    app.extend([
        app_image(),
        "serve".into(),
        "--data-dir".into(),
        "/data".into(),
    ]);
    run_docker(app)?;
    drop(app_socket);
    shutdown.check()?;
    docker(["start", &format!("{prefix}-app")])?;
    Ok(Endpoints {
        dashboard: format!(
            "http://127.0.0.1:{}",
            if host {
                app_port
            } else {
                published_port(&format!("{prefix}-app"), "8080/tcp")?
            }
        ),
        rpc: public_rpc.into(),
        lightwalletd: public_lwd,
        p2p: p2p.into(),
    })
}

pub(super) struct ExternalHost {
    rpc: String,
    networking: Networking,
}

impl ExternalHost {
    pub(super) fn new(runtime: &Runtime, name: &InstanceName, rpc: &str) -> Result<Self> {
        let rpc = local_rpc(rpc)?.to_string();
        let instance = runtime.read_instance(name)
            .with_context(|| format!("first run `ths --name {name} prepare --zakura-rpc {rpc}` and start Zakura using the printed configuration"))?;
        anyhow::ensure!(
            matches!(instance.node, NodeSource::ExternalRpc { rpc: ref expected, .. } if expected == &rpc),
            "instance {name} was prepared with different node settings; reset it explicitly before preparing another node"
        );
        Ok(Self {
            rpc,
            networking: Networking::discover()?,
        })
    }
}

impl StartHost for ExternalHost {
    fn external(&self) -> bool {
        true
    }
    fn prepare_start(&self, runtime: &Runtime, name: &InstanceName) -> Result<()> {
        runtime.detach_external(name)
    }
    fn delete(&self, runtime: &Runtime, name: &InstanceName) -> Result<()> {
        runtime.detach_external(name)
    }
    fn allocate(
        &self,
        runtime: &Runtime,
        name: &InstanceName,
        shutdown: &Shutdown,
    ) -> Result<Endpoints> {
        let mut instance = runtime.read_instance(name)?;
        let NodeSource::ExternalRpc { rpc, p2p, config } = &instance.node else {
            bail!("instance is not prepared for an external node");
        };
        anyhow::ensure!(
            rpc == &self.rpc,
            "external node settings changed; retry start"
        );
        let port = local_rpc(rpc)?
            .port_or_known_default()
            .context("missing RPC port")?;
        println!(
            "Attaching to {rpc}\n  Config: {}\n  Startup will fund the development wallet and mine on this Regtest chain.",
            config.display()
        );
        wait_for_rpc(rpc, shutdown, || Ok(()))?;
        if self.networking == Networking::Desktop {
            ensure_network(&prefix(name))?;
        }
        instance.endpoints = start_companions(
            name,
            self.networking,
            port,
            rpc,
            p2p,
            "external_rpc",
            shutdown,
        )?;
        runtime.save_instance(name, &instance)?;
        Ok(instance.endpoints)
    }
    fn wait_ready(
        &self,
        endpoints: &Endpoints,
        app: &str,
        timeout: Duration,
        shutdown: &Shutdown,
    ) -> Result<()> {
        wait_ready(
            &endpoints.dashboard,
            app,
            timeout.max(Duration::from_secs(600)),
            shutdown,
        )
    }
    fn open_url(&self, url: &str) -> Result<()> {
        open_url(url)
    }
    fn wait_for_shutdown(&self, shutdown: &Shutdown) -> Result<()> {
        shutdown.wait()
    }
}

fn local_rpc(value: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(value).context("invalid --zakura-rpc URL")?;
    anyhow::ensure!(
        url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost")),
        "--zakura-rpc must use http://127.0.0.1:<port> or http://localhost:<port>"
    );
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "--zakura-rpc must be a local RPC origin without credentials, path, query, or fragment"
    );
    url.set_host(Some("127.0.0.1"))?;
    Ok(url)
}

fn log_tail(path: &Path) -> String {
    (|| -> std::io::Result<String> {
        let mut file = File::open(path)?;
        let offset = file.metadata()?.len().saturating_sub(16_384);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })()
    .unwrap_or_else(|error| format!("could not read {}: {error}", path.display()))
}

pub(super) fn logs(path: &Path, follow: bool) -> Result<()> {
    anyhow::ensure!(
        path.is_file(),
        "native Zakura log is unavailable: {}",
        path.display()
    );
    let mut command = Command::new("tail");
    command.args(["-n", "100"]);
    if follow {
        command.arg("-f");
    }
    let status = command
        .arg(path)
        .status()
        .context("reading native Zakura logs")?;
    anyhow::ensure!(status.success(), "reading native Zakura logs failed");
    Ok(())
}

impl Runtime {
    pub fn prepare(&self, name: &InstanceName, rpc: &str, json: bool) -> Result<()> {
        let rpc = local_rpc(rpc)?.to_string();
        let _networking = Networking::discover()?;
        let _lock = self.lock_instance(name)?;
        if self.instance_dir(name).join("instance.json").exists() {
            let instance = self.read_instance(name)?;
            anyhow::ensure!(
                matches!(instance.node, NodeSource::ExternalRpc { rpc: ref expected, .. } if expected == &rpc),
                "instance {name} already exists with other node settings; reset it explicitly first"
            );
            return print_prepared(&instance, json);
        }
        require_image(&app_image())?;
        let dir = self.instance_dir(name);
        fs::create_dir_all(&dir)?;
        // Deliberately outside instance_dir: cleanup never removes developer-owned node files.
        let node_dir = self
            .root
            .join("external-nodes")
            .join(name.to_string())
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&node_dir)?;
        let node_dir = node_dir.canonicalize()?;
        let config = node_dir.join("zakurad.toml");
        let p2p_socket = TcpListener::bind("127.0.0.1:0")?;
        let p2p_port = p2p_socket.local_addr()?.port();
        let rpc_port = local_rpc(&rpc)?
            .port_or_known_default()
            .context("missing RPC port")?;
        anyhow::ensure!(
            rpc_port != p2p_port,
            "RPC and P2P port collision; retry prepare"
        );
        let instance = Instance {
            name: name.to_string(),
            version: 2,
            endpoints: Endpoints::default(),
            node: NodeSource::ExternalRpc {
                rpc: rpc.clone(),
                config: config.clone(),
                p2p: format!("127.0.0.1:{p2p_port}"),
            },
        };
        let prefix = prefix(name);
        // A failed prepare can be retried against the same wallet volume.
        for suffix in ["wallet", "config", "lightwalletd"] {
            ensure_volume(&format!("{prefix}-{suffix}"), name)?;
        }
        let init = format!("{prefix}-init");
        if container_exists(&init)? {
            docker(["rm", "-f", &init])?;
        }
        docker([
            "create",
            "--name",
            &init,
            "--label",
            &label(name),
            "-v",
            &format!("{prefix}-wallet:/data"),
            "-v",
            &format!("{prefix}-config:/config"),
            &app_image(),
            "init",
            "--defer-wallet",
        ])?;
        docker(["start", "-a", &init])?;
        docker([
            "cp",
            &format!("{init}:/config/zakurad.toml"),
            config.to_str().context("invalid config path")?,
        ])?;
        let template = fs::read_to_string(&config)?;
        fs::write(
            &config,
            native_config(&template, &node_dir.join("chain"), rpc_port, p2p_port)?,
        )?;
        self.save_instance(name, &instance)?;
        print_prepared(&instance, json)
    }

    pub(super) fn is_external(&self, name: &InstanceName) -> Result<bool> {
        if !self.instance_dir(name).join("instance.json").exists() {
            return Ok(false);
        }
        Ok(matches!(
            self.read_instance(name)?.node,
            NodeSource::ExternalRpc { .. }
        ))
    }

    pub(super) fn detach_external(&self, name: &InstanceName) -> Result<()> {
        anyhow::ensure!(
            self.is_external(name)?,
            "instance {name} is not externally managed"
        );
        let prefix = prefix(name);
        for service in ["app", "lightwalletd", "init"] {
            let target = format!("{prefix}-{service}");
            if container_exists(&target)? {
                docker(["rm", "-f", &target])?;
            }
        }
        if docker_output(["network", "inspect", &prefix]).is_ok() {
            docker(["network", "rm", &prefix])?;
        }
        let stop = self.instance_dir(name).join("stop-request");
        if stop.exists() {
            fs::remove_file(stop)?;
        }
        Ok(())
    }

    fn lock_file(&self, name: &InstanceName) -> Result<File> {
        fs::create_dir_all(&self.root)?;
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(format!("{name}.lock")))?)
    }

    pub(super) fn lock_instance(&self, name: &InstanceName) -> Result<InstanceLock> {
        let lock = self.lock_file(name)?;
        lock.try_lock().with_context(|| {
            format!("environment {name} is already running or being changed; stop it first")
        })?;
        Ok(InstanceLock(lock))
    }

    pub(super) fn stop_and_lock(&self, name: &InstanceName) -> Result<InstanceLock> {
        let lock = self.lock_file(name)?;
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(InstanceLock(lock)),
                Err(TryLockError::WouldBlock) => {
                    let dir = self.instance_dir(name);
                    if dir.is_dir() {
                        match fs::write(dir.join("stop-request"), b"stop") {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    anyhow::ensure!(
                        Instant::now() < deadline,
                        "environment {name} did not stop within 45 seconds; data was retained"
                    );
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub(super) fn save_instance(&self, name: &InstanceName, instance: &Instance) -> Result<()> {
        let dir = self.instance_dir(name);
        fs::write(
            dir.join("instance.json.tmp"),
            serde_json::to_vec_pretty(instance)?,
        )?;
        fs::rename(dir.join("instance.json.tmp"), dir.join("instance.json"))?;
        Ok(())
    }
}

fn print_prepared(instance: &Instance, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(instance)?);
    } else if let NodeSource::ExternalRpc { rpc, config, .. } = &instance.node {
        println!(
            "Prepared {}.\nStart your Zakura build with --config {:?} start\nThen run: ths --name {} start --zakura-rpc {}\nThe node configuration and chain remain yours when ths stops or resets.",
            instance.name, config, instance.name, rpc
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_config_preserves_treasury_and_escapes_host_paths() {
        let template = r#"
[network]
network = "Regtest"
listen_addr = "0.0.0.0:18233"
[network.testnet_parameters.activation_heights]
NU6 = 1
[rpc]
listen_addr = "0.0.0.0:18232"
enable_cookie_auth = false
[state]
cache_dir = "/data"
[mining]
miner_address = "treasury"
"#;
        let chain = Path::new("/tmp/build with spaces/quoted\"path/chain");
        let result = native_config(template, chain, 30001, 30002).unwrap();
        let config: toml::Value = toml::from_str(&result).unwrap();
        assert_eq!(config["state"]["cache_dir"].as_str(), chain.to_str());
        assert_eq!(config["mining"]["miner_address"].as_str(), Some("treasury"));
        assert_eq!(
            config["rpc"]["listen_addr"].as_str(),
            Some("127.0.0.1:30001")
        );
        assert_eq!(
            config["network"]["listen_addr"].as_str(),
            Some("127.0.0.1:30002")
        );
        assert_eq!(
            config["network"]["testnet_parameters"]["activation_heights"]["NU6"].as_integer(),
            Some(1)
        );
    }

    #[test]
    fn external_urls_cannot_redirect_to_remote_nodes_or_embed_secrets() {
        assert_eq!(
            local_rpc("http://localhost:18232").unwrap().as_str(),
            "http://127.0.0.1:18232/"
        );
        for url in [
            "https://127.0.0.1:18232",
            "http://example.com",
            "http://user:secret@127.0.0.1",
            "http://127.0.0.1/rpc",
            "http://127.0.0.1?key=secret",
            "http://127.0.0.1/#fragment",
        ] {
            assert!(local_rpc(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn local_modes_do_not_require_a_zakura_image() {
        assert!(
            !runtime_images(true)
                .iter()
                .any(|image| image == ZAKURA_IMAGE)
        );
        assert!(
            runtime_images(false)
                .iter()
                .any(|image| image == ZAKURA_IMAGE)
        );
    }

    #[test]
    fn supports_desktop_and_orbstack_on_linux_without_assuming_host_networking() {
        assert_eq!(
            Networking::from_environment("Docker Desktop", true, false).unwrap(),
            Networking::Desktop
        );
        assert_eq!(
            Networking::from_environment("OrbStack", false, false).unwrap(),
            Networking::Desktop
        );
        assert_eq!(
            Networking::from_environment("Ubuntu", true, false).unwrap(),
            Networking::Host
        );
        assert!(Networking::from_environment("Ubuntu", true, true).is_err());
        assert!(Networking::from_environment("Unrecognized VM", false, false).is_err());
        assert_eq!(Networking::Host.rpc_host(), "127.0.0.1");
    }

    #[test]
    fn reads_legacy_metadata_as_docker_owned() {
        let value = serde_json::json!({"name":"old","version":1,"endpoints":{"dashboard":"a","rpc":"b","lightwalletd":"c","p2p":"d"}});
        let instance: Instance = serde_json::from_value(value).unwrap();
        assert!(matches!(instance.node, NodeSource::Docker));
    }

    #[test]
    fn instance_locks_exclude_competing_starts_and_release_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime {
            root: dir.path().into(),
        };
        let name: InstanceName = "locking".parse().unwrap();
        let lock = runtime.lock_instance(&name).unwrap();
        assert!(runtime.lock_instance(&name).is_err());
        drop(lock);
        runtime.lock_instance(&name).unwrap();
    }

    #[test]
    fn stop_request_interrupts_readiness_without_a_signal() {
        let dir = tempfile::tempdir().unwrap();
        let (_sender, receiver) = mpsc::channel();
        let mut shutdown = Shutdown::from_receiver(receiver);
        let marker = dir.path().join("stop-request");
        shutdown.stop_request = Some(marker.clone());
        fs::write(marker, "stop").unwrap();
        assert!(shutdown.check().is_err());
        shutdown.wait().unwrap();
    }

    #[test]
    fn stale_identity_never_terminates_another_process() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let actual = ProcessIdentity::capture(child.id()).unwrap();
        let stale = ProcessIdentity {
            pid: actual.pid,
            identity: "a previous process".into(),
        };
        stale.stop().unwrap();
        assert!(child.try_wait().unwrap().is_none());
        actual.stop().unwrap();
        assert!(!child.wait().unwrap().success());
    }

    #[test]
    fn log_tail_handles_large_and_non_utf8_output() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("zakura.log");
        let mut content = vec![0xff; 20_000];
        content.extend_from_slice(b"last diagnostic");
        fs::write(&log, content).unwrap();
        assert!(log_tail(&log).ends_with("last diagnostic"));
    }
}
