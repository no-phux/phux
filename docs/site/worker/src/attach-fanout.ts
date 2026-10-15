// Fan-out bookkeeping for the sockets attached to one demo session.
//
// One session, several screens: a second view is another copy of the same
// terminals, so a server frame belongs to the whole session unless the wire
// addresses it to one attachment (see ./wire.ts).
//
// Two rules beyond routing:
//
//   • Only the newest attachment may change the shared terminal geometry. The
//     edge shell has one terminal and no per-view sizing, and `attach` applies
//     the attaching client's viewport to that terminal, so a screen that just
//     attached must not be resized out from under it by a view that attached
//     earlier. This is the tmux rule: the newest/smallest screen wins.
//   • An attachment-addressed frame whose attachment is unknown is dropped, not
//     broadcast: no other view can do anything useful with it.
//
// A durable object hibernates between messages and comes back with a fresh
// instance, so this state is evidence-based rather than authoritative: `seq`
// (connection order) and the attach ids a socket has opened are persisted in the
// socket's own attachment, and the caller re-registers them on wake.

import {
  attachIdOf,
  frameType,
  TYPE_ATTACH,
  TYPE_BOOTSTRAP_BEGIN,
  TYPE_BOOTSTRAP_CHUNK,
  TYPE_BOOTSTRAP_READY,
  TYPE_COMMAND_RESULT,
  TYPE_DETACHED,
  TYPE_ERROR,
  TYPE_HELLO_OK,
  TYPE_PONG,
  TYPE_RESIZE_TERMINAL,
} from "./wire";

/**
 * Frames that answer the connection that asked, not the session: a handshake
 * reply, a command result keyed by the requester's correlation id, or the
 * bootstrap one view streams for itself. A second view either cannot use them
 * (its request ids are its own) or is actively wrong to receive them — a
 * broadcast `HELLO_OK` is a protocol error to a client that already greeted,
 * which is how a second view once took the first one down.
 */
const PER_CONNECTION = new Set([
  TYPE_HELLO_OK,
  TYPE_ERROR,
  TYPE_COMMAND_RESULT,
  TYPE_PONG,
  TYPE_DETACHED,
  TYPE_BOOTSTRAP_BEGIN,
  TYPE_BOOTSTRAP_CHUNK,
  TYPE_BOOTSTRAP_READY,
]);

export class ViewFanout<T> {
  private readonly views = new Map<T, number>();
  /** Attach ids each socket opened, so removal can reclaim them. */
  private readonly opened = new Map<T, Set<number>>();
  /** Attach id → the socket that opened it. */
  private readonly owners = new Map<number, T>();

  /** Register a socket. `seq` is its connection order; higher is newer. */
  add(socket: T, seq: number): void {
    this.views.set(socket, seq);
    this.opened.set(socket, new Set());
  }

  /** Whether this socket is already registered. */
  has(socket: T): boolean {
    return this.views.has(socket);
  }

  /** Forget a socket; its attach ids stop being routable. */
  remove(socket: T): void {
    if (!this.views.delete(socket)) return;
    for (const attachId of this.opened.get(socket) ?? []) {
      if (this.owners.get(attachId) === socket) this.owners.delete(attachId);
    }
    this.opened.delete(socket);
  }

  /** Remember an attachment a socket opened, after hibernation too. */
  claim(socket: T, attachId: number): void {
    if (!this.views.has(socket)) return;
    this.opened.get(socket)?.add(attachId);
    this.owners.set(attachId, socket);
  }

  /** Every attached view, oldest connection first. */
  all(): T[] {
    return [...this.views.entries()]
      .sort(([, left], [, right]) => left - right)
      .map(([socket]) => socket);
  }

  get count(): number {
    return this.views.size;
  }

  /**
   * Record a client frame. Returns whether it should reach the session: an
   * attach records its correlation id, and a shared-terminal resize from a view
   * that no longer owns the geometry is dropped.
   */
  fromClient(socket: T, frame: Uint8Array): boolean {
    if (!this.views.has(socket)) return false;
    const type = frameType(frame);
    const attachId = attachIdOf(frame);
    if (type === TYPE_ATTACH && attachId !== undefined) {
      this.claim(socket, attachId);
    }
    if (type === TYPE_RESIZE_TERMINAL && !this.ownsGeometry(socket)) {
      return false;
    }
    return true;
  }

  /** Where one server frame goes: one socket, every view, or nowhere. */
  targets(frame: Uint8Array, sender: T): T[] {
    const type = frameType(frame);
    // Replies to the connection that asked: they answer this view's handshake or
    // describe this view's bootstrap, so another view must never see them.
    if (type !== undefined && PER_CONNECTION.has(type)) return [sender];
    const attachId = attachIdOf(frame);
    if (attachId === undefined) return this.all();
    const owner = this.owners.get(attachId);
    if (owner !== undefined && this.views.has(owner)) return [owner];
    return [];
  }

  /** The newest attachment owns the shared terminal geometry. */
  private ownsGeometry(socket: T): boolean {
    if (this.views.size < 2) return true;
    const newest = this.views.get(socket) ?? -1;
    for (const seq of this.views.values()) {
      if (seq > newest) return false;
    }
    return true;
  }
}
