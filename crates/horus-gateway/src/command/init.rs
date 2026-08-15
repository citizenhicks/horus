use super::*;

pub(super) fn initialize(options: InitOptions) -> Result<()> {
    let (store, config) = match options.cloudflare {
        Some(CloudflareInit::Quick) => {
            ConfigStore::initialize_quick_cloudflare(options.state_dir, options.listen)?
        }
        Some(CloudflareInit::Named { hostname, token }) => {
            ConfigStore::initialize_named_cloudflare(
                options.state_dir,
                options.listen,
                &hostname,
                &token,
            )?
        }
        None if options.tls.is_none() => {
            ConfigStore::initialize_quick_cloudflare(options.state_dir, options.listen)?
        }
        None => ConfigStore::initialize(options.state_dir, options.listen, options.tls)?,
    };
    initialize_auth(&store)?;
    println!("initialized Horus gateway");
    print_listener(&config, None);
    println!("run `horus-gateway connect` to pair a client");
    Ok(())
}

pub(super) fn initialize_auth(store: &ConfigStore) -> Result<()> {
    if let Err(error) = AuthStore::initialize(store.auth_path()) {
        std::fs::remove_dir_all(store.state_dir()).map_err(|cleanup| {
            Error::Config(format!(
                "{error}; failed to remove incomplete gateway state at {}: {cleanup}",
                store.state_dir().display()
            ))
        })?;
        return Err(error);
    }
    Ok(())
}

pub(super) fn provision_cloudflare_local_client(
    auth: &AuthStore,
    config: &GatewayConfig,
) -> Result<Option<(Endpoint, String)>> {
    if config.cloudflare.is_none() {
        return Ok(None);
    }
    let endpoint = loopback_endpoint(config)?;
    let issued = auth.provision_local_client()?;
    Ok(Some((endpoint, issued.token)))
}

pub(super) fn loopback_endpoint(config: &GatewayConfig) -> Result<Endpoint> {
    format!("tcp://{}", config.listen).parse()
}

/// Initializes one gateway with an account-free Cloudflare Quick Tunnel.
pub fn initialize_quick_cloudflare(state_dir: PathBuf) -> Result<()> {
    initialize(InitOptions {
        state_dir,
        listen: DEFAULT_LISTEN,
        tls: None,
        cloudflare: Some(CloudflareInit::Quick),
    })
}

/// Initializes one gateway against a user-owned named Cloudflare Tunnel.
pub fn initialize_named_cloudflare(
    state_dir: PathBuf,
    hostname: String,
    token: String,
) -> Result<()> {
    initialize(InitOptions {
        state_dir,
        listen: DEFAULT_LISTEN,
        tls: None,
        cloudflare: Some(CloudflareInit::Named { hostname, token }),
    })
}

/// Permanently removes previously confirmed gateway state after stopping its process.
///
/// # Errors
///
/// Returns an error unless the target is an empty real directory or contains a regular
/// `gateway.toml` marker. Lifecycle or filesystem failures are also returned.
pub fn reset_gateway_state(state_dir: PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        let had_config = validate_reset_target(&state_dir, false)?;
        let state_dir = fs::canonicalize(state_dir)?;
        let _startup = StartupGuard::create(&state_dir)?;
        if validate_reset_target(&state_dir, true)? != had_config {
            return Err(invalid_reset_target(&state_dir));
        }
        stop_gateway(&state_dir, None)?;
        fs::remove_dir_all(state_dir)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = state_dir;
        Err(unsupported_lifecycle())
    }
}

#[cfg(unix)]
pub(super) fn validate_reset_target(path: &Path, ignore_startup_lock: bool) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_reset_target(path));
    }
    let mut empty = true;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if ignore_startup_lock && entry.file_name() == STARTUP_FILE {
            continue;
        }
        empty = false;
    }
    if empty {
        return Ok(false);
    }
    let marker = fs::symlink_metadata(path.join(STATE_MARKER_FILE))
        .map_err(|_| invalid_reset_target(path))?;
    if !marker.is_file() || marker.file_type().is_symlink() {
        return Err(invalid_reset_target(path));
    }
    Ok(true)
}

#[cfg(unix)]
pub(super) fn invalid_reset_target(path: &Path) -> Error {
    Error::Config(format!(
        "refusing to reset {}: expected an empty directory or Horus gateway state with a regular {STATE_MARKER_FILE}",
        path.display()
    ))
}
