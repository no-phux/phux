//! `phux host add NAME --code LINK`: registering from a connect link.

use std::io::Read as _;
use std::process::ExitCode;

use phux_client_runtime::pairing::MAX_CONNECT_LINK_BYTES;

use super::super::json_err::{self, CliError, codes};
use super::super::remote_target::{self, RemoteTarget};

use super::args::AddOpts;
use super::enrollment::{EnrollmentReport, report_registered};
use super::list::HostRow;

/// Register `raw_name` from `--code`, failing when the link's ticket does not
/// redeem: what `phux attach --remote NAME --code LINK` registers, without
/// the attach, for a caller that dials the host itself.
pub(super) fn run_add_from_code(raw_name: &str, opts: &AddOpts) -> ExitCode {
    let json = opts.json.json;
    let target = match RemoteTarget::parse_labeled(raw_name, "host add") {
        Ok(target) => target,
        Err(err) => {
            let remedy = "name the machine with a plain word: `phux host add mini --code LINK`";
            return json_err::emit(json, &CliError::new(codes::REGISTRY, err, remedy), 2);
        }
    };
    let link = match read_link(opts.code.as_deref().unwrap_or_default()) {
        Ok(link) => link,
        Err(err) => return json_err::emit(json, &code_error(err), 2),
    };
    match remote_target::pair_from_code(&target, &link) {
        Ok((entry, status)) => report_registered(
            &HostRow::from_remote(entry),
            "from the connect link",
            &[],
            json,
            None,
            &EnrollmentReport::new(status),
        ),
        Err(err) => json_err::emit(json, &code_error(err), 1),
    }
}

/// The link itself, or `-` for one read from stdin.
fn read_link(arg: &str) -> Result<String, String> {
    if arg != "-" {
        return Ok(arg.to_owned());
    }
    let mut link = String::new();
    let limit = u64::try_from(MAX_CONNECT_LINK_BYTES + 1).unwrap_or(u64::MAX);
    std::io::stdin()
        .take(limit)
        .read_to_string(&mut link)
        .map_err(|err| format!("--code -: could not read the link from stdin: {err}"))?;
    // The parser refuses an oversized link without echoing it.
    Ok(link.trim_end_matches(['\n', '\r']).to_owned())
}

fn code_error(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "mint a new link with `phux pair` on that machine (`--relay-route ROUTE` for a relay), \
         then run `phux host add NAME --code LINK` again",
    )
}
