use racc_identity::{
    CliConfig, ConnectionPath, IdentityError, SystemProcessRunner, TailscaleClient,
};
use std::net::IpAddr;
use std::time::{Duration, Instant};

const RUNS: usize = 20;

fn failure_class(error: &IdentityError) -> &'static str {
    match error {
        IdentityError::NotInstalled => "not-installed",
        IdentityError::NotRunning => "not-running",
        IdentityError::NotLoggedIn => "not-logged-in",
        IdentityError::Timeout => "timeout",
        IdentityError::OutputTooLarge => "output-too-large",
        IdentityError::BadJson => "bad-json",
        IdentityError::Io(_) => "io-error",
        IdentityError::CommandFailed(_) => "command-failed",
        IdentityError::AddressNotTailscale => "address-not-tailscale",
        IdentityError::NoTailscaleAddress => "no-address",
        IdentityError::AmbiguousWhois => "ambiguous-whois",
        IdentityError::InvalidIdentity => "invalid-identity",
        IdentityError::TooManyPeers => "too-many-peers",
        IdentityError::InvalidDiscoveryConfig => "invalid-discovery-config",
        IdentityError::DiscoveryWorkerFailed => "discovery-worker-failed",
        IdentityError::Allowlist(_) => "allowlist-error",
    }
}

fn percentile(mut values: Vec<u128>, numerator: usize, denominator: usize) -> u128 {
    values.sort_unstable();
    let index = values
        .len()
        .saturating_sub(1)
        .saturating_mul(numerator)
        .div_ceil(denominator);
    values.get(index).copied().unwrap_or_default()
}

fn timings<T>(
    mut operation: impl FnMut() -> Result<T, IdentityError>,
) -> Result<(u128, u128), &'static str> {
    let mut samples = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let start = Instant::now();
        operation().map_err(|error| failure_class(&error))?;
        samples.push(start.elapsed().as_micros());
    }
    let median = percentile(samples.clone(), 50, 100);
    let p95 = percentile(samples, 95, 100);
    Ok((median, p95))
}

fn main() {
    let Some(executable) = racc_identity::locate_tailscale_cli() else {
        println!("tailscale=not-installed");
        return;
    };
    let client = TailscaleClient::with_executable(
        executable,
        SystemProcessRunner,
        CliConfig {
            command_timeout: Duration::from_secs(2),
            status_ttl: Duration::ZERO,
            whois_ttl: Duration::ZERO,
            ..CliConfig::default()
        },
    );
    match client.version() {
        Ok(version) => println!(
            "tailscale_version={}",
            version
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect::<String>()
        ),
        Err(error) => {
            println!("tailscale_version_error={}", failure_class(&error));
            return;
        }
    }
    let snapshot = match client.status() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            println!("status_error={}", failure_class(&error));
            return;
        }
    };
    let online = snapshot.peers.iter().filter(|peer| peer.online).count();
    let (direct, derp, unknown) = snapshot.peers.iter().filter(|peer| peer.online).fold(
        (0usize, 0usize, 0usize),
        |(direct, derp, unknown), peer| match peer.path {
            ConnectionPath::Direct => (direct + 1, derp, unknown),
            ConnectionPath::Derp { .. } => (direct, derp + 1, unknown),
            ConnectionPath::Unknown => (direct, derp, unknown + 1),
        },
    );
    println!(
        "status=ok peers={} online={} path_direct={} path_derp={} path_unknown={}",
        snapshot.peers.len(),
        online,
        direct,
        derp,
        unknown
    );
    println!(
        "self_bind_policy_accepted={}",
        client.self_bind_addr().is_ok()
    );
    match timings(|| client.status()) {
        Ok((median, p95)) => println!(
            "status_cli_calls={} median_us={} p95_us={}",
            RUNS, median, p95
        ),
        Err(error) => println!("status_timing_error={error}"),
    }
    let whois_address: Option<IpAddr> = snapshot
        .peers
        .iter()
        .find(|peer| peer.online)
        .or_else(|| snapshot.peers.first())
        .and_then(|peer| peer.addresses.first().copied());
    match whois_address {
        Some(address) => match timings(|| client.resolve(address)) {
            Ok((median, p95)) => println!(
                "whois=ok calls={} median_us={} p95_us={}",
                RUNS, median, p95
            ),
            Err(error) => println!("whois_error={error}"),
        },
        None => println!("whois=not-run no-peer-address"),
    }
}
