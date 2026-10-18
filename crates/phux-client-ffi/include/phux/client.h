#ifndef PHUX_CLIENT_H
#define PHUX_CLIENT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PHUX_CLIENT_ABI_VERSION 3u
/* Version 3 (the domain model): every PhuxWorkspace* record and function is
 * now PhuxArrangement* (window fields are group fields), the shared
 * arrangement is a compare-and-swap of the session's stored arrangement,
 * PhuxResourceInfo gained trailing fields, session creation takes a
 * PhuxSessionLifetime, and the organization section below is new.
 * PhuxSessionInfo is frozen: PhuxSessionRecord carries what it lacks. Every
 * sized record still requires version == PHUX_CLIENT_ABI_VERSION. */
/* Additive members of an open enum (PhuxClientStatusKind, PhuxClientDamageKind,
 * PhuxClientAgentRecordsKind, and similarly documented "additive" kinds) do
 * not bump this version: a host must already tolerate a member it does not
 * recognise by skipping that effect. Only a struct layout or function
 * signature change bumps PHUX_CLIENT_ABI_VERSION. */
#define PHUX_CLIENT_MAX_OUTBOUND_BYTES (64u * 1024u)
#define PHUX_CLIENT_RELEASE_CARGO_PROFILE "ffi-release"

/* Supported C/C++ interface. Ship this header with its adjacent detail tree;
 * the sections are internal and retain declaration order under this umbrella. */
#include "detail/input.h"
#include "detail/types.h"
#include "detail/effects.h"
#include "detail/grid.h"
#include "detail/client.h"
#include "detail/arrangement.h"
#include "detail/terminal.h"
#include "detail/paths.h"
#include "detail/sessions.h"
#include "detail/organization.h"
#include "detail/connection.h"
#include "detail/remote.h"

#ifdef __cplusplus
}
#endif
#endif
