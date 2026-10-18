#ifndef PHUX_CLIENT_DETAIL_REMOTE_H
#define PHUX_CLIENT_DETAIL_REMOTE_H

/* Internal section of phux/client.h; include the umbrella header instead. */

/* ---------------------------------------------------------- remote hosts
 *
 * Reach a remote phux server the way `phux attach --remote HOST` does
 * (ADR-0007, ADR-0031, ADR-0093 rung 1) without implementing QUIC or TLS.
 * Additive to ABI version 2; no existing declaration changes.
 *
 * PhuxClient stays sans-IO. The embedder creates a connected SOCK_STREAM
 * Unix-domain socket pair, keeps one end for exactly the framed I/O it
 * already does against a local server's socket, and hands the other end to
 * a tunnel. The tunnel dials the host and relays SPEC section 5 frames
 * through the pair unchanged; nothing is decoded.
 *
 * Hosts come from the CLI's own [[remote]] registry in the phux config.toml
 * (`phux host add|enroll`, `phux --remote`); there is no second registry.
 * A target is a registry name or [USER@]HOST[:PORT], matched as the CLI
 * matches it: the exact name, then the bare host, then any entry whose
 * endpoint addresses that host. An explicit :PORT overrides the endpoint for
 * this dial only. quic:// and wss:// (ws:// on loopback) are dialed; ssh://
 * is refused because it needs a terminal. Pairing is never attempted: an
 * unregistered host fails with a message naming the CLI command that pairs
 * it. The bearer token is read from the entry's token file inside the tunnel
 * and never crosses this ABI. Trust matches the CLI: a routable host needs
 * its certificate pin, and a routable WebSocket also needs wss:// and a
 * token. Name resolution plus establishment is bounded to 15 seconds.
 *
 * Linking: on macOS the static archive now also references CoreFoundation
 * (the registry is read through phux-config, whose clock dependency asks
 * CoreFoundation for the system time zone). Link -framework CoreFoundation;
 * an AppKit application already does.
 */
typedef struct PhuxRemoteTunnel PhuxRemoteTunnel;

typedef enum PhuxRemoteTunnelState {
    PHUX_REMOTE_TUNNEL_RESOLVED = 0,   /* registry entry found; not started */
    PHUX_REMOTE_TUNNEL_CONNECTING = 1, /* dialing */
    PHUX_REMOTE_TUNNEL_CONNECTED = 2,  /* relaying frames */
    PHUX_REMOTE_TUNNEL_FAILED = 3,     /* terminal; message says why */
    PHUX_REMOTE_TUNNEL_CLOSED = 4      /* terminal; embedder end closed or freed */
} PhuxRemoteTunnelState;

typedef enum PhuxRemoteTransport {
    PHUX_REMOTE_TRANSPORT_NONE = 0,
    PHUX_REMOTE_TRANSPORT_QUIC = 1,
    PHUX_REMOTE_TRANSPORT_WS = 2
} PhuxRemoteTransport;

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * target: UTF-8 without NUL, at most 1024 bytes. config_path: empty for the
 * CLI's resolution ($XDG_CONFIG_HOME/phux/config.toml, else
 * ~/.config/phux/config.toml), otherwise an absolute path. */
typedef struct PhuxRemoteTarget {
    size_t size;
    uint32_t version;
    PhuxBytes target;
    PhuxBytes config_path;
} PhuxRemoteTarget;

/** Initialize size = sizeof(struct), version = PHUX_CLIENT_ABI_VERSION.
 * Spans are borrowed from the tunnel until phux_remote_tunnel_free. name is
 * the registry entry (the typed target when resolution failed); endpoint is
 * the effective URI after any :PORT override; session is the entry's pinned
 * session or empty; message is empty unless state is FAILED. */
typedef struct PhuxRemoteTunnelInfo {
    size_t size;
    uint32_t version;
    uint32_t state;
    uint32_t transport;
    PhuxBytes name;
    PhuxBytes endpoint;
    PhuxBytes session;
    PhuxBytes message;
} PhuxRemoteTunnelInfo;

/** Resolve without touching the network: reads config.toml only. The token
 * file is read by the tunnel thread just before it dials, and the owned
 * copy is dropped as soon as the dial completes. Returns PHUX_CLIENT_OK with a tunnel even for an unregistered
 * host; that tunnel is FAILED with a message. Only malformed arguments fail
 * the call. */
PhuxClientResult phux_remote_tunnel_resolve(const PhuxRemoteTarget *target, PhuxRemoteTunnel **out_tunnel);
/** Fresh RESOLVED tunnel from the source's immutable captured dial configuration.
 * Copies endpoint, pin and token-file/config provenance without registry or token
 * reads. Shares no socket, thread, cancellation or connection state. Source may
 * be connecting/failed; it must have resolved successfully and remain live for
 * this call (no concurrent free). Caller owns out; writable out is cleared on
 * failure. Ordinary reconnect/independent attachments use this exact snapshot;
 * an explicit registry Retry may resolve anew to adopt changed credentials. */
PhuxClientResult phux_remote_tunnel_clone_resolved(const PhuxRemoteTunnel *source, PhuxRemoteTunnel **out_tunnel);
/** Thread-safe with respect to the tunnel's own thread and to a concurrent
 * phux_remote_tunnel_start; may be called from any thread until free. */
PhuxClientResult phux_remote_tunnel_info(const PhuxRemoteTunnel *tunnel, PhuxRemoteTunnelInfo *out_info);
/** Requires RESOLVED; otherwise PHUX_CLIENT_INVALID_STATE. Ownership of
 * transport_fd transfers on EVERY return path, including failure. On macOS
 * set SO_NOSIGPIPE on both ends first: the tunnel writes from a library
 * thread and cannot change the process's SIGPIPE disposition. On failure the
 * tunnel publishes FAILED and its message BEFORE closing transport_fd, so
 * the embedder reads EOF only after the reason is readable. */
PhuxClientResult phux_remote_tunnel_start(PhuxRemoteTunnel *tunnel, int transport_fd);
/** Cancels any dial, closes the connection, and joins the tunnel thread;
 * bounded by one scheduler poll, never by the network. Must not race info. */
void phux_remote_tunnel_free(PhuxRemoteTunnel *tunnel);

/** Start a session against the exact dial a PhuxRemoteTunnel already
 * resolved, without reading the registry again.
 *
 * This is the Machines path. A captured tunnel retains the endpoint, pin and
 * token provenance of the row the user chose, so a later reconnect still
 * reaches that host even if the registry alias has since been retargeted.
 * The tunnel is BORROWED, not consumed, and nothing is relayed through it:
 * only its resolved configuration is read, so the caller still frees it with
 * phux_remote_tunnel_free. options.target and options.socket_path must both
 * be empty, because the tunnel is the destination. The wake has the same
 * inline-registration and context-lifetime contract as phux_client_connect. */
PhuxClientResult phux_client_connect_captured(const PhuxRemoteTunnel *tunnel, const PhuxConnectOptions *options, PhuxClient **out_client);


/* Saved-machine inventory, additive ABI v2. All calls are single-owner-thread.
 * Listing reads only configuration (including its layers), never credentials or
 * network. No four-entry limit: caller budgets bound capture and indexes page it.
 * Every span below is borrowed until registry_free. Initialize size/version.
 * Rows have no connection state: the host joins authenticated endpoint identity.
 * Captured (registry handle,index) is authority; display names alone are not.
 */
typedef struct PhuxMachineRegistry PhuxMachineRegistry;
typedef struct PhuxMachineRegistryOptions {
    size_t size;
    uint32_t version;
    PhuxBytes config_path; /* Empty selects the CLI path; otherwise absolute. */
    size_t max_entries;
    /* Aggregate root + unique inherited file bytes; embedded defaults excluded. */
    size_t max_file_bytes;
} PhuxMachineRegistryOptions;
typedef struct PhuxMachineRegistryInfo {
    size_t size;
    uint32_t version;
    size_t count;
    uint32_t failed;
    PhuxBytes message;
} PhuxMachineRegistryInfo;
/* Display spans are at most 1024 UTF-8 bytes. Oversized identity fields produce
 * an unsupported repair row. Never authorize actions from elided display text. */
typedef struct PhuxMachineRecord {
    size_t size;
    uint32_t version;
    uint32_t role; /* 1 remote, 2 satellite */
    uint32_t route; /* 1 direct, 2 setup, 3 via hub, 4 disabled, 5 unsupported */
    PhuxBytes name;
    PhuxBytes endpoint;
    PhuxBytes session;
    PhuxBytes message;
} PhuxMachineRecord;
/* Load errors yield OK with info.failed=1 and a sanitized actionable message. */
PhuxClientResult phux_machine_registry_open(const PhuxMachineRegistryOptions *options, PhuxMachineRegistry **out);
PhuxClientResult phux_machine_registry_info(const PhuxMachineRegistry *registry, PhuxMachineRegistryInfo *out);
PhuxClientResult phux_machine_registry_get(const PhuxMachineRegistry *registry, size_t index, PhuxMachineRecord *out);
/* State error if captured root bytes or effective registrations changed. */
PhuxClientResult phux_machine_registry_validate(const PhuxMachineRegistry *registry, size_t index);
/* Returns a checked, exact tunnel (no network yet). Start never resolves aliases
 * again. Caller owns out through phux_remote_tunnel_free. Unsupported roles fail. */
PhuxClientResult phux_machine_registry_resolve(const PhuxMachineRegistry *registry, size_t index, PhuxRemoteTunnel **out);
/* Caller first explicitly disconnects. Exact root entry only, comments preserved;
 * no token files removed. Success invalidates capture; refresh before next action. */
PhuxClientResult phux_machine_registry_forget(const PhuxMachineRegistry *registry, size_t index);
void phux_machine_registry_free(PhuxMachineRegistry *registry);


#endif
