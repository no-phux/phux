#ifndef PHUX_DETAIL_APPROVALS_H
#define PHUX_DETAIL_APPROVALS_H

/* Opt in, then pump/poll normally. Approval-only: no 1Password invocation or values.
 * JSON snapshots contain schema_version=1, supported, approvals. Show untrusted
 * purpose, exact references, requester and bound provider client/credential_id;
 * client_name and request.provider are labels, never authenticated identities.
 * The provider desktop unlocks 1Password locally even after remote approval.
 * All output spans are borrowed until the next mutable client call.
 * Decide sends once; drain decision notifications for decided/stale/refused/
 * unconfirmed. Never retry automatically. Reconcile if dropped is true. */
PhuxClientResult phux_client_watch_approvals(PhuxClient *client);
PhuxClientResult phux_client_refresh_approvals(PhuxClient *client);
PhuxClientResult phux_client_approvals_json(PhuxClient *client, PhuxBytes *out);
PhuxClientResult phux_client_decide_approval(PhuxClient *client, PhuxBytes id, bool approve);
PhuxClientResult phux_client_approval_decisions_take_json(PhuxClient *client, PhuxBytes *out);

#endif
