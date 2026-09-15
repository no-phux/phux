export type PresentationPhase = "opening" | "presented" | "closing" | "retired";
export type WindowSlot = 0 | 1 | 2 | 3 | 4;

export interface FocusReturn {
  readonly kind: "workspace";
  readonly window: WindowSlot;
}

interface OwnedPresentation {
  readonly owner: WindowSlot;
  readonly phase: PresentationPhase;
  readonly focusReturn: FocusReturn;
  readonly continuation: number;
}

/// The typed interpretation used by lifecycle transitions. The model stores
/// its contract-safe encoding below; invalid wire-shaped records decode to
/// `none`, so reducers and projections never handle an impossible variant.
interface NoPresentation { readonly kind: "none"; }
interface NavigatorPresentation extends OwnedPresentation { readonly kind: "navigator"; readonly view: number; readonly inspector: boolean; }
interface SettingsPresentation extends OwnedPresentation { readonly kind: "settings"; }
interface HostPresentation extends OwnedPresentation { readonly kind: "host"; }
interface DirectoryPresentation extends OwnedPresentation { readonly kind: "directory"; }
interface RenamePresentation extends OwnedPresentation { readonly kind: "rename"; readonly creation: boolean; }

export type PresentationSurface = NoPresentation | NavigatorPresentation | SettingsPresentation |
  HostPresentation | DirectoryPresentation | RenamePresentation;

export type PresentationIntent =
  | { readonly kind: "none" }
  | { readonly kind: "navigator"; readonly view: number; readonly inspector: boolean; readonly phase: PresentationPhase }
  | { readonly kind: "settings"; readonly phase: PresentationPhase }
  | { readonly kind: "host"; readonly phase: PresentationPhase }
  | { readonly kind: "directory"; readonly phase: PresentationPhase }
  | { readonly kind: "rename"; readonly creation: boolean; readonly phase: PresentationPhase };

/// Contract-safe value record persisted in Model. `kind` is none/navigator/
/// settings/host/directory/rename (0..5); `phase` is opening/presented/closing/
/// retired (0..3). Only this module encodes or interprets those discriminants.
export interface PresentationLifecycle {
  readonly kind: number;
  readonly owner: number;
  readonly phase: number;
  readonly focusReturnKind: number;
  readonly focusReturnWindow: number;
  readonly focusReturnView: number;
  readonly navigatorView: number;
  readonly inspector: boolean;
  readonly creation: boolean;
  readonly continuation: number;
  readonly retiredWindows: number;
  readonly emptyWindows: number;
  readonly snapshotObserved: boolean;
  readonly snapshotActiveWindow: number;
  readonly nextContinuation: number;
}

export interface PresentationProjection {
  readonly mainPaletteOpen: boolean; readonly window1PaletteOpen: boolean; readonly window2PaletteOpen: boolean; readonly window3PaletteOpen: boolean; readonly window4PaletteOpen: boolean;
  readonly mainAgentsOpen: boolean; readonly window1AgentsOpen: boolean; readonly window2AgentsOpen: boolean; readonly window3AgentsOpen: boolean; readonly window4AgentsOpen: boolean;
  readonly mainSettingsOpen: boolean; readonly window1SettingsOpen: boolean; readonly window2SettingsOpen: boolean; readonly window3SettingsOpen: boolean; readonly window4SettingsOpen: boolean;
  readonly mainHostOpen: boolean; readonly window1HostOpen: boolean; readonly window2HostOpen: boolean; readonly window3HostOpen: boolean; readonly window4HostOpen: boolean;
  readonly mainDirOpen: boolean; readonly window1DirOpen: boolean; readonly window2DirOpen: boolean; readonly window3DirOpen: boolean; readonly window4DirOpen: boolean;
  readonly mainRenameOpen: boolean; readonly window1RenameOpen: boolean; readonly window2RenameOpen: boolean; readonly window3RenameOpen: boolean; readonly window4RenameOpen: boolean;
  readonly mainEmptyOpen: boolean; readonly window1EmptyOpen: boolean; readonly window2EmptyOpen: boolean; readonly window3EmptyOpen: boolean; readonly window4EmptyOpen: boolean;
}

export function initialPresentation(): PresentationLifecycle {
  return { kind: 0, owner: 0, phase: 0, focusReturnKind: 0, focusReturnWindow: 0, focusReturnView: 0,
    navigatorView: 0, inspector: false, creation: false, continuation: 0,
    retiredWindows: 0, emptyWindows: 0, snapshotObserved: false,
    snapshotActiveWindow: 0, nextContinuation: 1 };
}

function windowSlot(window: number): WindowSlot {
  if (window === 1) return 1;
  if (window === 2) return 2;
  if (window === 3) return 3;
  if (window === 4) return 4;
  return 0;
}

function validWindowSlot(window: number): boolean {
  return window === 0 || window === 1 || window === 2 || window === 3 || window === 4;
}

function phaseFrom(code: number): PresentationPhase {
  if (code === 1) return "presented";
  if (code === 2) return "closing";
  if (code === 3) return "retired";
  return "opening";
}

function owned(lifecycle: PresentationLifecycle) {
  let owner: WindowSlot = 0;
  if (lifecycle.owner === 1) owner = 1;
  else if (lifecycle.owner === 2) owner = 2;
  else if (lifecycle.owner === 3) owner = 3;
  else if (lifecycle.owner === 4) owner = 4;
  let focusWindow: WindowSlot = 0;
  if (lifecycle.focusReturnWindow === 1) focusWindow = 1;
  else if (lifecycle.focusReturnWindow === 2) focusWindow = 2;
  else if (lifecycle.focusReturnWindow === 3) focusWindow = 3;
  else if (lifecycle.focusReturnWindow === 4) focusWindow = 4;
  return { owner, phase: phaseFrom(lifecycle.phase),
    focusReturn: { kind: "workspace" as const, window: focusWindow },
    continuation: lifecycle.continuation };
}

function navigatorSurface(lifecycle: PresentationLifecycle): NavigatorPresentation {
  const base = owned(lifecycle);
  return { ...base, kind: "navigator", view: lifecycle.navigatorView, inspector: lifecycle.inspector };
}

function settingsSurface(lifecycle: PresentationLifecycle): SettingsPresentation {
  const base = owned(lifecycle);
  return { ...base, kind: "settings" };
}

function hostSurface(lifecycle: PresentationLifecycle): HostPresentation {
  const base = owned(lifecycle);
  return { ...base, kind: "host" };
}

function directorySurface(lifecycle: PresentationLifecycle): DirectoryPresentation {
  const base = owned(lifecycle);
  return { ...base, kind: "directory" };
}

function renameSurface(lifecycle: PresentationLifecycle): RenamePresentation {
  const base = owned(lifecycle);
  return { ...base, kind: "rename", creation: lifecycle.creation };
}

export function presentationSurface(lifecycle: PresentationLifecycle): PresentationSurface {
  if (!validWindowSlot(lifecycle.owner)) return { kind: "none" };
  if (!validWindowSlot(lifecycle.focusReturnWindow)) return { kind: "none" };
  if (lifecycle.phase !== 0 && lifecycle.phase !== 1 && lifecycle.phase !== 2 && lifecycle.phase !== 3) return { kind: "none" };
  if (lifecycle.focusReturnKind !== 0) return { kind: "none" };
  if (lifecycle.kind === 1) return navigatorSurface(lifecycle);
  if (lifecycle.kind === 2) return settingsSurface(lifecycle);
  if (lifecycle.kind === 3) return hostSurface(lifecycle);
  if (lifecycle.kind === 4) return directorySurface(lifecycle);
  if (lifecycle.kind === 5) return renameSurface(lifecycle);
  return { kind: "none" };
}

function intentKind(intent: PresentationIntent): number {
  if (intent.kind === "navigator") return 1;
  if (intent.kind === "settings") return 2;
  if (intent.kind === "host") return 3;
  if (intent.kind === "directory") return 4;
  if (intent.kind === "rename") return 5;
  return 0;
}

function intentPhase(intent: PresentationIntent): PresentationPhase {
  if (intent.kind === "navigator") return intent.phase;
  if (intent.kind === "settings") return intent.phase;
  if (intent.kind === "host") return intent.phase;
  if (intent.kind === "directory") return intent.phase;
  if (intent.kind === "rename") return intent.phase;
  return "opening";
}

function ownerCanPresent(lifecycle: PresentationLifecycle, owner: number, openWindows: number): boolean {
  if (!validWindowSlot(owner)) return false;
  if ((lifecycle.retiredWindows & windowBit(owner)) !== 0) return false;
  if ((openWindows & windowBit(owner)) !== 0) return true;
  if (!lifecycle.snapshotObserved) return true;
  return lifecycle.snapshotActiveWindow === owner;
}

function encodeIntent(lifecycle: PresentationLifecycle, intent: PresentationIntent, owner: WindowSlot, continuation: number): PresentationLifecycle {
  if (intent.kind === "none") return { ...lifecycle, kind: 0, continuation: 0 };
  const rawView = intent.kind === "navigator" ? intent.view : 0;
  const view = rawView >= 0 && rawView <= 9007199254740991 ? Math.trunc(rawView) : 0;
  const inspector = intent.kind === "navigator" && intent.inspector;
  const creation = intent.kind === "rename" && intent.creation;
  const kind = intent.kind === "navigator" ? 1 : intent.kind === "settings" ? 2 : intent.kind === "host" ? 3 : intent.kind === "directory" ? 4 : 5;
  const requestedPhase = intentPhase(intent);
  const phase = requestedPhase === "presented" ? 1 : requestedPhase === "closing" ? 2 : requestedPhase === "retired" ? 3 : 0;
  const encodedOwner = owner >= 0 && owner <= 4 ? Math.trunc(owner) : 0;
  const encodedContinuation = continuation >= 0 && continuation <= 9007199254740991 ? Math.trunc(continuation) : 0;
  return { ...lifecycle, kind, owner: encodedOwner, phase,
    focusReturnKind: 0, focusReturnWindow: encodedOwner, focusReturnView: 0,
    navigatorView: view, inspector, creation, continuation: encodedContinuation };
}

function sameSurface(lifecycle: PresentationLifecycle, intent: PresentationIntent): boolean {
  if (lifecycle.kind !== intentKind(intent)) return false;
  if (intent.kind === "navigator") return lifecycle.inspector === intent.inspector && lifecycle.navigatorView === intent.view;
  if (intent.kind === "rename") return lifecycle.creation === intent.creation;
  return true;
}

function capturePresentation(lifecycle: PresentationLifecycle, intent: PresentationIntent, activeWindow: number): PresentationLifecycle {
  const continuation = lifecycle.nextContinuation >= 1 && lifecycle.nextContinuation < 9007199254740991 ? Math.trunc(lifecycle.nextContinuation) : 1;
  return { ...encodeIntent(lifecycle, intent, windowSlot(activeWindow), continuation),
    nextContinuation: continuation + 1 };
}

export function reconcilePresentation(lifecycle: PresentationLifecycle, intent: PresentationIntent, activeWindow: number, openWindows: number): PresentationLifecycle {
  if (intent.kind === "none") return encodeIntent(lifecycle, intent, 0, 0);
  if (!sameSurface(lifecycle, intent)) {
    const captured = capturePresentation(lifecycle, intent, activeWindow);
    return ownerCanPresent(lifecycle, activeWindow, openWindows) ? captured : { ...captured, phase: 3 };
  }
  if (!ownerCanPresent(lifecycle, lifecycle.owner, openWindows)) {
    return { ...encodeIntent(lifecycle, intent, windowSlot(lifecycle.owner), lifecycle.continuation), phase: 3 };
  }
  const requestedPhase = intentPhase(intent);
  const phase = lifecycle.phase === 3 ? 3 : requestedPhase === "presented" ? 1 : requestedPhase === "closing" ? 2 : requestedPhase === "retired" ? 3 : 0;
  return { ...encodeIntent(lifecycle, intent, windowSlot(lifecycle.owner), lifecycle.continuation), phase };
}

function windowBit(window: number): number {
  if (window === 1) return 2;
  if (window === 2) return 4;
  if (window === 3) return 8;
  if (window === 4) return 16;
  return 1;
}

export function presentationOwnedBy(lifecycle: PresentationLifecycle, window: number): boolean {
  return lifecycle.kind !== 0 && validWindowSlot(window) && lifecycle.owner === windowSlot(window);
}

export function retirePresentationWindow(lifecycle: PresentationLifecycle, window: number): PresentationLifecycle {
  if (window !== 1 && window !== 2 && window !== 3 && window !== 4) return lifecycle;
  const retiredWindows = lifecycle.retiredWindows | windowBit(window);
  if (!presentationOwnedBy(lifecycle, window)) return { ...lifecycle, retiredWindows };
  const continuation = lifecycle.nextContinuation >= 1 && lifecycle.nextContinuation < 9007199254740991 ? Math.trunc(lifecycle.nextContinuation) : 1;
  return { ...lifecycle, retiredWindows, phase: 3, continuation,
    nextContinuation: continuation + 1 };
}

export function syncPresentationSnapshot(lifecycle: PresentationLifecycle, openWindows: number, emptyWindows: number, activeWindow: number): PresentationLifecycle {
  const empty = emptyWindows >= 0 && emptyWindows <= 31 ? Math.trunc(emptyWindows) : 0;
  const active = activeWindow >= 0 && activeWindow <= 4 ? Math.trunc(activeWindow) : 0;
  return { ...lifecycle, retiredWindows: lifecycle.retiredWindows & openWindows,
    emptyWindows: empty, snapshotObserved: true, snapshotActiveWindow: active };
}

export function windowIsDeclared(lifecycle: PresentationLifecycle, window: number, open: boolean): boolean {
  if (!open) return false;
  return (lifecycle.retiredWindows & windowBit(window)) === 0;
}

function visibleOwned(surface: OwnedPresentation): number {
  return surface.phase === "retired" ? -1 : surface.owner;
}

function visibleOwner(surface: PresentationSurface): number {
  if (surface.kind === "navigator") return visibleOwned(surface);
  if (surface.kind === "settings") return visibleOwned(surface);
  if (surface.kind === "host") return visibleOwned(surface);
  if (surface.kind === "directory") return visibleOwned(surface);
  if (surface.kind === "rename") return visibleOwned(surface);
  return -1;
}

function owns(surface: PresentationSurface, kind: PresentationSurface["kind"], window: number): boolean {
  if (visibleOwner(surface) !== window) return false;
  return surface.kind === kind;
}

function ownsNavigator(surface: PresentationSurface, inspector: boolean, window: number): boolean {
  if (!owns(surface, "navigator", window)) return false;
  return surface.kind === "navigator" && surface.inspector === inspector;
}

function showsEmpty(lifecycle: PresentationLifecycle, surface: PresentationSurface, window: number): boolean {
  if (visibleOwner(surface) !== -1) return false;
  const bit = windowBit(window);
  if ((lifecycle.retiredWindows & bit) !== 0) return false;
  return (lifecycle.emptyWindows & bit) !== 0;
}

export function projectPresentation(lifecycle: PresentationLifecycle): PresentationProjection {
  const surface = presentationSurface(lifecycle);
  return {
    mainPaletteOpen: ownsNavigator(surface, false, 0), window1PaletteOpen: ownsNavigator(surface, false, 1), window2PaletteOpen: ownsNavigator(surface, false, 2), window3PaletteOpen: ownsNavigator(surface, false, 3), window4PaletteOpen: ownsNavigator(surface, false, 4),
    mainAgentsOpen: ownsNavigator(surface, true, 0), window1AgentsOpen: ownsNavigator(surface, true, 1), window2AgentsOpen: ownsNavigator(surface, true, 2), window3AgentsOpen: ownsNavigator(surface, true, 3), window4AgentsOpen: ownsNavigator(surface, true, 4),
    mainSettingsOpen: owns(surface, "settings", 0), window1SettingsOpen: owns(surface, "settings", 1), window2SettingsOpen: owns(surface, "settings", 2), window3SettingsOpen: owns(surface, "settings", 3), window4SettingsOpen: owns(surface, "settings", 4),
    mainHostOpen: owns(surface, "host", 0), window1HostOpen: owns(surface, "host", 1), window2HostOpen: owns(surface, "host", 2), window3HostOpen: owns(surface, "host", 3), window4HostOpen: owns(surface, "host", 4),
    mainDirOpen: owns(surface, "directory", 0), window1DirOpen: owns(surface, "directory", 1), window2DirOpen: owns(surface, "directory", 2), window3DirOpen: owns(surface, "directory", 3), window4DirOpen: owns(surface, "directory", 4),
    mainRenameOpen: owns(surface, "rename", 0), window1RenameOpen: owns(surface, "rename", 1), window2RenameOpen: owns(surface, "rename", 2), window3RenameOpen: owns(surface, "rename", 3), window4RenameOpen: owns(surface, "rename", 4),
    mainEmptyOpen: showsEmpty(lifecycle, surface, 0), window1EmptyOpen: showsEmpty(lifecycle, surface, 1), window2EmptyOpen: showsEmpty(lifecycle, surface, 2), window3EmptyOpen: showsEmpty(lifecycle, surface, 3), window4EmptyOpen: showsEmpty(lifecycle, surface, 4),
  };
}
