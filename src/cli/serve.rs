use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

use super::OutputConfig;

#[derive(Args)]
#[command(
    after_help = "Set BOBBIN_INDEX_WRITER_LOCK to an index wrapper's existing lock file to defer opportunistic stats compaction while that writer is active. Missing or unsafe configured locks defer compaction; queries remain available."
)]
pub struct ServeArgs {
    /// Directory to serve (defaults to current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Run HTTP server instead of MCP stdio server
    #[arg(long)]
    http: bool,

    /// HTTP server port (default: 3030)
    #[arg(long, default_value = "3030")]
    port: u16,

    /// Run MCP server alongside HTTP server
    #[arg(long)]
    mcp: bool,

    /// Run MCP server over HTTP transport (Streamable HTTP, network-accessible)
    #[arg(long)]
    mcp_http: bool,

    /// Port for MCP HTTP transport (default: 3031)
    #[arg(long, default_value = "3031")]
    mcp_port: u16,

    /// Serve the local index only, ignoring any configured bobbin server.
    ///
    /// Use this when the local index is the one you mean — a repo-scoped MCP
    /// server on a machine that also has a global `[server].url`.
    #[arg(long)]
    no_remote: bool,
}

/// Resolve the bobbin server this MCP server should proxy to, if any.
///
/// `output.server` is the CLI's own resolution — `--server` / `BOBBIN_SERVER`
/// > repo config > global config — so the MCP surface and the CLI surface now
/// answer from the same corpus. They did not before: `bobbin connect` wrote
/// the URL, the CLI honoured it and `bobbin serve` ignored it, so the MCP
/// tools every agent actually uses answered from an empty or repo-local index
/// while `bobbin search` answered from the fleet index (aegis-wbbycq).
///
/// Two deliberate exceptions:
///
/// * `--no-remote` — an explicit local-only override.
/// * `--http` — this process is SERVING the HTTP API, so it is the origin of
///   the index, not a client of one. The fleet's own index server runs
///   `bobbin serve --http --port 3000 --mcp-http --mcp-port 3031` on a host
///   whose global config names the very route that fronts it; without this
///   guard that server would proxy its own MCP tools back to itself through
///   the load balancer.
fn resolve_remote(args: &ServeArgs, output: &OutputConfig) -> Option<String> {
    if args.no_remote || args.http {
        return None;
    }
    output.server.clone()
}

pub async fn run(args: ServeArgs, output: OutputConfig) -> Result<()> {
    let remote = resolve_remote(&args, &output);
    let role = output.role.clone();

    // With a remote configured the directory need not be a bobbin repo at all,
    // so a non-existent path is the only thing worth rejecting here. The old
    // unconditional `canonicalize` was fine because a local index was always
    // required; now "." in a scratch directory is a legitimate invocation.
    let repo_root = args
        .path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Invalid path: {}", e))?;

    match (args.http, args.mcp, args.mcp_http) {
        // HTTP API + MCP stdio: run both concurrently
        (true, true, false) => {
            let http_root = repo_root.clone();
            let http_port = args.port;
            let mcp_root = repo_root;

            tokio::select! {
                result = crate::http::run_server(http_root, http_port) => {
                    result?;
                }
                result = crate::mcp::run_server(mcp_root, remote, role) => {
                    result?;
                }
            }

            Ok(())
        }

        // HTTP API + MCP over HTTP: run both concurrently
        (true, _, true) => {
            let http_root = repo_root.clone();
            let http_port = args.port;
            let mcp_root = repo_root;
            let mcp_port = args.mcp_port;

            tokio::select! {
                result = crate::http::run_server(http_root, http_port) => {
                    result?;
                }
                result = crate::mcp::run_http_server(mcp_root, mcp_port, remote, role) => {
                    result?;
                }
            }

            Ok(())
        }

        // MCP over HTTP only
        (false, _, true) => {
            crate::mcp::run_http_server(repo_root, args.mcp_port, remote, role).await
        }

        // HTTP API only
        (true, false, false) => crate::http::run_server(repo_root, args.port).await,

        // MCP stdio only (default)
        _ => crate::mcp::run_server(repo_root, remote, role).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(http: bool, no_remote: bool) -> ServeArgs {
        ServeArgs {
            path: PathBuf::from("."),
            http,
            port: 3030,
            mcp: false,
            mcp_http: false,
            mcp_port: 3031,
            no_remote,
        }
    }

    fn output(server: Option<&str>) -> OutputConfig {
        OutputConfig {
            json: false,
            quiet: false,
            verbose: false,
            server: server.map(str::to_string),
            role: "default".to_string(),
        }
    }

    #[test]
    fn stdio_mcp_uses_the_configured_server() {
        assert_eq!(
            resolve_remote(&args(false, false), &output(Some("http://search.example"))),
            Some("http://search.example".to_string())
        );
    }

    #[test]
    fn no_server_configured_stays_local() {
        assert_eq!(resolve_remote(&args(false, false), &output(None)), None);
    }

    #[test]
    fn no_remote_flag_overrides_the_configured_server() {
        assert_eq!(
            resolve_remote(&args(false, true), &output(Some("http://search.example"))),
            None
        );
    }

    /// The self-proxy guard. A process serving the HTTP API is the index
    /// origin; if it also honoured `[server].url` it would proxy its own MCP
    /// tools back through the route that fronts it.
    #[test]
    fn an_http_index_server_never_proxies_to_itself() {
        assert_eq!(
            resolve_remote(&args(true, false), &output(Some("http://search.example"))),
            None
        );
    }
}
