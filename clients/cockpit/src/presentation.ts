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
  readonly absentRetiredWindows: number;
  readonly emptyWindows: number;
  readonly snapshotObserved: boolean;
  readonly snapshotActiveWindow: number;
  readonly retirementSequenceHi: number;
  readonly retirementSequenceLo: number;
  readonly retirementRevisionHi: number;
  readonly retirementRevisionLo: number;
  readonly absenceSequenceHi: number;
  readonly absenceSequenceLo: number;
  readonly absenceRevisionHi: number;
  readonly absenceRevisionLo: number;
  readonly nextContinuation: number;
}

export interface PresentationAuthority {
  readonly sequenceHi: number;
  readonly sequenceLo: number;
  readonly revisionHi: number;
  readonly revisionLo: number;
}

export interface PresentationSnapshotObservation extends PresentationAuthority {
  readonly openWindows: number;
  readonly emptyWindows: number;
  readonly activeWindow: number;
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
    retiredWindows: 0, absentRetiredWindows: 0, emptyWindows: 0, snapshotObserved: false,
    snapshotActiveWindow: 0,
    retirementSequenceHi: 0, retirementSequenceLo: 0, retirementRevisionHi: 0, retirementRevisionLo: 0,
    absenceSequenceHi: 0, absenceSequenceLo: 0, absenceRevisionHi: 0, absenceRevisionLo: 0,
    nextContinuation: 1 };
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
  const owner = (lifecycle.owner >= 0 && lifecycle.owner <= 4 ? Math.trunc(lifecycle.owner) : 0) as WindowSlot;
  const focusWindow = (lifecycle.focusReturnWindow >= 0 && lifecycle.focusReturnWindow <= 4 ? Math.trunc(lifecycle.focusReturnWindow) : 0) as WindowSlot;
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

function validPresentationPhase(phase: number): boolean {
  return phase === 0 || phase === 1 || phase === 2 || phase === 3;
}

function wholeBetween(value: number, minimum: number, maximum: number): boolean {
  return value >= minimum && value <= maximum && Math.trunc(value) === value;
}

function validPresentationAuthority(authority: PresentationAuthority): boolean {
  if (!wholeBetween(authority.sequenceHi, 0, 4294967295)) return false;
  if (!wholeBetween(authority.sequenceLo, 0, 4294967295)) return false;
  if (!wholeBetween(authority.revisionHi, 0, 4294967295)) return false;
  return wholeBetween(authority.revisionLo, 0, 4294967295);
}

function validAuthority(lifecycle: PresentationLifecycle): boolean {
  if (!validPresentationAuthority({ sequenceHi: lifecycle.retirementSequenceHi, sequenceLo: lifecycle.retirementSequenceLo,
    revisionHi: lifecycle.retirementRevisionHi, revisionLo: lifecycle.retirementRevisionLo })) return false;
  return validPresentationAuthority({ sequenceHi: lifecycle.absenceSequenceHi, sequenceLo: lifecycle.absenceSequenceLo,
    revisionHi: lifecycle.absenceRevisionHi, revisionLo: lifecycle.absenceRevisionLo });
}

function validWindowMasks(lifecycle: PresentationLifecycle): boolean {
  if (!wholeBetween(lifecycle.retiredWindows, 0, 31)) return false;
  if ((lifecycle.retiredWindows & 1) !== 0) return false;
  if (!wholeBetween(lifecycle.absentRetiredWindows, 0, 31)) return false;
  if ((lifecycle.absentRetiredWindows & lifecycle.retiredWindows) !== lifecycle.absentRetiredWindows) return false;
  return wholeBetween(lifecycle.emptyWindows, 0, 31);
}

function validSnapshotMetadata(lifecycle: PresentationLifecycle): boolean {
  if (!validWindowSlot(lifecycle.snapshotActiveWindow)) return false;
  if (!lifecycle.snapshotObserved && lifecycle.snapshotActiveWindow !== 0) return false;
  return lifecycle.snapshotObserved === true || lifecycle.snapshotObserved === false;
}

function validPresentationMetadata(lifecycle: PresentationLifecycle): boolean {
  if (!validWindowSlot(lifecycle.owner)) return false;
  if (!validWindowSlot(lifecycle.focusReturnWindow)) return false;
  if (!validPresentationPhase(lifecycle.phase)) return false;
  if (!validWindowMasks(lifecycle)) return false;
  if (!validSnapshotMetadata(lifecycle)) return false;
  if (lifecycle.inspector !== true && lifecycle.inspector !== false) return false;
  if (lifecycle.creation !== true && lifecycle.creation !== false) return false;
  if (!wholeBetween(lifecycle.nextContinuation, 1, 9007199254740991)) return false;
  if (!validAuthority(lifecycle)) return false;
  return lifecycle.focusReturnKind === 0;
}

function canonicalNone(lifecycle: PresentationLifecycle): boolean {
  if (lifecycle.owner !== 0 || lifecycle.phase !== 0 || lifecycle.focusReturnWindow !== 0) return false;
  if (lifecycle.focusReturnView !== 0 || lifecycle.navigatorView !== 0) return false;
  if (lifecycle.inspector || lifecycle.creation) return false;
  return lifecycle.continuation === 0;
}

function canonicalOwned(lifecycle: PresentationLifecycle): boolean {
  if (lifecycle.owner !== lifecycle.focusReturnWindow) return false;
  if (!wholeBetween(lifecycle.continuation, 1, 9007199254740991)) return false;
  if (lifecycle.nextContinuation <= lifecycle.continuation) return false;
  return lifecycle.focusReturnView === 0;
}

function canonicalKindFields(lifecycle: PresentationLifecycle): boolean {
  if (lifecycle.kind === 1) {
    if (!wholeBetween(lifecycle.navigatorView, 0, 4)) return false;
    if (lifecycle.inspector && lifecycle.navigatorView !== 0) return false;
    return !lifecycle.creation;
  }
  if (lifecycle.navigatorView !== 0 || lifecycle.inspector) return false;
  if (lifecycle.kind === 5) return true;
  return !lifecycle.creation;
}

function validEncodedPresentation(lifecycle: PresentationLifecycle): boolean {
  if (!validPresentationMetadata(lifecycle)) return false;
  if (lifecycle.kind === 0) return canonicalNone(lifecycle);
  if (!wholeBetween(lifecycle.kind, 1, 5)) return false;
  if (!canonicalOwned(lifecycle)) return false;
  return canonicalKindFields(lifecycle);
}

/// ScriptC persists structural records in Model, so an opaque/private brand
/// cannot cross the compiled module seam. Every exported authority function
/// validates this one canonical encoding instead.
export function presentationLifecycleValid(lifecycle: PresentationLifecycle): boolean {
  return validEncodedPresentation(lifecycle);
}

export function presentationSurface(lifecycle: PresentationLifecycle): PresentationSurface {
  if (!validEncodedPresentation(lifecycle)) return { kind: "none" };
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
  if (!wholeBetween(openWindows, 1, 31)) return false;
  if ((lifecycle.retiredWindows & windowBit(owner)) !== 0) return false;
  if (owner === 0) return true;
  return (openWindows & windowBit(owner)) !== 0;
}

function phaseCode(phase: PresentationPhase): number {
  if (phase === "presented") return 1;
  if (phase === "closing") return 2;
  if (phase === "retired") return 3;
  return 0;
}

function noneEncoding(lifecycle: PresentationLifecycle): PresentationLifecycle {
  return { ...lifecycle, kind: 0, owner: 0, phase: 0, focusReturnKind: 0, focusReturnWindow: 0,
    focusReturnView: 0, navigatorView: 0, inspector: false, creation: false, continuation: 0 };
}

interface OwnedEncoding {
  readonly owner: number;
  readonly phase: number;
  readonly continuation: number;
}

function ownedEncoding(owner: WindowSlot, continuation: number, requested: PresentationPhase): OwnedEncoding {
  const encodedOwner = owner >= 0 && owner <= 4 ? Math.trunc(owner) : 0;
  const encodedContinuation = continuation >= 1 && continuation <= 9007199254740991 ? Math.trunc(continuation) : 1;
  const rawPhase = phaseCode(requested);
  const phase = rawPhase >= 0 && rawPhase <= 3 ? Math.trunc(rawPhase) : 0;
  return { owner: encodedOwner, phase, continuation: encodedContinuation };
}

function encodedLifecycle(lifecycle: PresentationLifecycle, owned: OwnedEncoding, kindValue: number, viewValue: number, inspector: boolean, creation: boolean): PresentationLifecycle {
  const kind = kindValue >= 1 && kindValue <= 5 ? Math.trunc(kindValue) : 1;
  const owner = owned.owner >= 0 && owned.owner <= 4 ? Math.trunc(owned.owner) : 0;
  const phase = owned.phase >= 0 && owned.phase <= 3 ? Math.trunc(owned.phase) : 0;
  const continuation = owned.continuation >= 1 && owned.continuation <= 9007199254740991 ? Math.trunc(owned.continuation) : 1;
  const navigatorView = viewValue >= 0 && viewValue <= 4 ? Math.trunc(viewValue) : 0;
  return { ...lifecycle, kind, owner, phase, focusReturnKind: 0, focusReturnWindow: owner, focusReturnView: 0,
    navigatorView, inspector, creation, continuation };
}

function encodeIntent(lifecycle: PresentationLifecycle, intent: PresentationIntent, owner: WindowSlot, continuation: number): PresentationLifecycle {
  if (intent.kind === "none") return noneEncoding(lifecycle);
  const encoded = ownedEncoding(owner, continuation, intentPhase(intent));
  if (intent.kind === "navigator") return encodedLifecycle(lifecycle, encoded, 1, intent.view, intent.inspector, false);
  if (intent.kind === "settings") return encodedLifecycle(lifecycle, encoded, 2, 0, false, false);
  if (intent.kind === "host") return encodedLifecycle(lifecycle, encoded, 3, 0, false, false);
  if (intent.kind === "directory") return encodedLifecycle(lifecycle, encoded, 4, 0, false, false);
  return encodedLifecycle(lifecycle, encoded, 5, 0, false, intent.creation);
}

function sameSurface(surface: PresentationSurface, intent: PresentationIntent): boolean {
  if (surface.kind !== intent.kind) return false;
  if (surface.kind === "navigator" && intent.kind === "navigator") return surface.inspector === intent.inspector && surface.view === intent.view;
  if (surface.kind === "rename" && intent.kind === "rename") return surface.creation === intent.creation;
  return true;
}

function capturePresentation(lifecycle: PresentationLifecycle, intent: PresentationIntent, activeWindow: number): PresentationLifecycle {
  const continuation = lifecycle.nextContinuation >= 1 && lifecycle.nextContinuation < 9007199254740991 ? Math.trunc(lifecycle.nextContinuation) : 1;
  return { ...encodeIntent(lifecycle, intent, windowSlot(activeWindow), continuation),
    nextContinuation: continuation + 1 };
}

export function reconcilePresentation(lifecycle: PresentationLifecycle, intent: PresentationIntent, activeWindow: number, openWindows: number): PresentationLifecycle {
  if (!validEncodedPresentation(lifecycle)) lifecycle = initialPresentation();
  if (intent.kind === "none") return encodeIntent(lifecycle, intent, 0, 0);
  if (!sameSurface(presentationSurface(lifecycle), intent)) {
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
  const surface = presentationSurface(lifecycle);
  if (surface.kind === "none" || !validWindowSlot(window)) return false;
  return visibleOwner(surface) === windowSlot(window);
}

export function presentationAcceptsReply(lifecycle: PresentationLifecycle, kind: PresentationSurface["kind"], continuation: number): boolean {
  const surface = presentationSurface(lifecycle);
  if (surface.kind === "none" || surface.kind !== kind) return false;
  return surfaceContinuation(surface) === continuation && continuation > 0;
}

export function presentationContinuation(lifecycle: PresentationLifecycle, kind: PresentationSurface["kind"]): number {
  const surface = presentationSurface(lifecycle);
  if (surface.kind === "none" || surface.kind !== kind) return 0;
  return surfaceContinuation(surface);
}

function authorityNewer(observed: PresentationAuthority, sequenceHi: number, sequenceLo: number, revisionHi: number, revisionLo: number): boolean {
  if (observed.sequenceHi !== sequenceHi) return observed.sequenceHi > sequenceHi;
  if (observed.sequenceLo !== sequenceLo) return observed.sequenceLo > sequenceLo;
  if (observed.revisionHi !== revisionHi) return observed.revisionHi > revisionHi;
  return observed.revisionLo > revisionLo;
}

function encodedAuthority(authority: PresentationAuthority): PresentationAuthority {
  const sequenceHi = authority.sequenceHi >= 0 && authority.sequenceHi <= 4294967295 ? Math.trunc(authority.sequenceHi) : 0;
  const sequenceLo = authority.sequenceLo >= 0 && authority.sequenceLo <= 4294967295 ? Math.trunc(authority.sequenceLo) : 0;
  const revisionHi = authority.revisionHi >= 0 && authority.revisionHi <= 4294967295 ? Math.trunc(authority.revisionHi) : 0;
  const revisionLo = authority.revisionLo >= 0 && authority.revisionLo <= 4294967295 ? Math.trunc(authority.revisionLo) : 0;
  return { sequenceHi, sequenceLo, revisionHi, revisionLo };
}

function withRetirementMasks(lifecycle: PresentationLifecycle, retiredValue: number, absentValue: number): PresentationLifecycle {
  const retiredWindows = retiredValue >= 0 && retiredValue <= 30 ? Math.trunc(retiredValue) : 0;
  const absentRetiredWindows = absentValue >= 0 && absentValue <= 30 ? Math.trunc(absentValue) : 0;
  return { ...lifecycle, retiredWindows, absentRetiredWindows };
}

function withRetirementAuthority(lifecycle: PresentationLifecycle, retiredWindows: number, absentRetiredWindows: number, authority: PresentationAuthority): PresentationLifecycle {
  const sequenceHi = authority.sequenceHi >= 0 && authority.sequenceHi <= 4294967295 ? Math.trunc(authority.sequenceHi) : 0;
  const sequenceLo = authority.sequenceLo >= 0 && authority.sequenceLo <= 4294967295 ? Math.trunc(authority.sequenceLo) : 0;
  const revisionHi = authority.revisionHi >= 0 && authority.revisionHi <= 4294967295 ? Math.trunc(authority.revisionHi) : 0;
  const revisionLo = authority.revisionLo >= 0 && authority.revisionLo <= 4294967295 ? Math.trunc(authority.revisionLo) : 0;
  const masked = withRetirementMasks(lifecycle, retiredWindows, absentRetiredWindows);
  return { ...masked,
    retirementSequenceHi: sequenceHi, retirementSequenceLo: sequenceLo,
    retirementRevisionHi: revisionHi, retirementRevisionLo: revisionLo };
}

export function retirePresentationWindow(lifecycle: PresentationLifecycle, window: number, authority: PresentationAuthority): PresentationLifecycle {
  if (!validEncodedPresentation(lifecycle)) lifecycle = initialPresentation();
  if (!validPresentationAuthority(authority)) return lifecycle;
  if (window !== 1 && window !== 2 && window !== 3 && window !== 4) return lifecycle;
  const retiredWindows = lifecycle.retiredWindows | windowBit(window);
  const retired = withRetirementAuthority(lifecycle, retiredWindows,
    lifecycle.absentRetiredWindows & ~windowBit(window), authority);
  if (!presentationOwnedBy(lifecycle, window)) return retired;
  const continuation = lifecycle.nextContinuation >= 1 && lifecycle.nextContinuation < 9007199254740991 ? Math.trunc(lifecycle.nextContinuation) : 1;
  return { ...retired, phase: 3, continuation,
    nextContinuation: continuation + 1 };
}

function validSnapshotObservation(observation: PresentationSnapshotObservation): boolean {
  if (!wholeBetween(observation.openWindows, 1, 31)) return false;
  if (!wholeBetween(observation.emptyWindows, 0, 31)) return false;
  if (!validWindowSlot(observation.activeWindow)) return false;
  if (observation.activeWindow > 0 && (observation.openWindows & windowBit(observation.activeWindow)) === 0) return false;
  if (!wholeBetween(observation.sequenceHi, 0, 4294967295)) return false;
  if (!wholeBetween(observation.sequenceLo, 0, 4294967295)) return false;
  if (!wholeBetween(observation.revisionHi, 0, 4294967295)) return false;
  return wholeBetween(observation.revisionLo, 0, 4294967295);
}

function encodedSnapshotObservation(observation: PresentationSnapshotObservation): PresentationSnapshotObservation {
  const openWindows = observation.openWindows >= 1 && observation.openWindows <= 31 ? Math.trunc(observation.openWindows) : 1;
  const emptyWindows = observation.emptyWindows >= 0 && observation.emptyWindows <= 31 ? Math.trunc(observation.emptyWindows) : 0;
  const activeWindow = observation.activeWindow >= 0 && observation.activeWindow <= 4 ? Math.trunc(observation.activeWindow) : 0;
  const authority = encodedAuthority(observation);
  return { openWindows, emptyWindows, activeWindow, sequenceHi: authority.sequenceHi, sequenceLo: authority.sequenceLo,
    revisionHi: authority.revisionHi, revisionLo: authority.revisionLo };
}

function absenceAuthority(lifecycle: PresentationLifecycle, observation: PresentationSnapshotObservation, newlyAbsent: number): PresentationAuthority {
  if (newlyAbsent !== 0) return encodedAuthority(observation);
  return encodedAuthority({ sequenceHi: lifecycle.absenceSequenceHi, sequenceLo: lifecycle.absenceSequenceLo,
    revisionHi: lifecycle.absenceRevisionHi, revisionLo: lifecycle.absenceRevisionLo });
}

function applySnapshotMetadata(lifecycle: PresentationLifecycle, observation: PresentationSnapshotObservation, absence: PresentationAuthority, retiredWindows: number, absentRetiredWindows: number): PresentationLifecycle {
  const emptyWindows = observation.emptyWindows >= 0 && observation.emptyWindows <= 31 ? Math.trunc(observation.emptyWindows) : 0;
  const activeWindow = observation.activeWindow >= 0 && observation.activeWindow <= 4 ? Math.trunc(observation.activeWindow) : 0;
  const sequenceHi = absence.sequenceHi >= 0 && absence.sequenceHi <= 4294967295 ? Math.trunc(absence.sequenceHi) : 0;
  const sequenceLo = absence.sequenceLo >= 0 && absence.sequenceLo <= 4294967295 ? Math.trunc(absence.sequenceLo) : 0;
  const revisionHi = absence.revisionHi >= 0 && absence.revisionHi <= 4294967295 ? Math.trunc(absence.revisionHi) : 0;
  const revisionLo = absence.revisionLo >= 0 && absence.revisionLo <= 4294967295 ? Math.trunc(absence.revisionLo) : 0;
  const masked = withRetirementMasks(lifecycle, retiredWindows, absentRetiredWindows);
  return { ...masked,
    emptyWindows, snapshotObserved: true, snapshotActiveWindow: activeWindow,
    absenceSequenceHi: sequenceHi, absenceSequenceLo: sequenceLo,
    absenceRevisionHi: revisionHi, absenceRevisionLo: revisionLo };
}

export function syncPresentationSnapshot(lifecycle: PresentationLifecycle, observation: PresentationSnapshotObservation): PresentationLifecycle {
  if (!validEncodedPresentation(lifecycle)) lifecycle = initialPresentation();
  if (!validSnapshotObservation(observation)) return initialPresentation();
  const observed = encodedSnapshotObservation(observation);
  const afterRetirement = authorityNewer(observed, lifecycle.retirementSequenceHi, lifecycle.retirementSequenceLo,
    lifecycle.retirementRevisionHi, lifecycle.retirementRevisionLo);
  const afterAbsence = authorityNewer(observed, lifecycle.absenceSequenceHi, lifecycle.absenceSequenceLo,
    lifecycle.absenceRevisionHi, lifecycle.absenceRevisionLo);
  const newlyAbsent = afterRetirement ? lifecycle.retiredWindows & ~observed.openWindows : 0;
  const reincarnated = afterAbsence ? lifecycle.absentRetiredWindows & observed.openWindows : 0;
  const absentRetiredWindows = (lifecycle.absentRetiredWindows | newlyAbsent) & ~reincarnated;
  const absence = absenceAuthority(lifecycle, observed, newlyAbsent);
  return applySnapshotMetadata(lifecycle, observed, absence,
    lifecycle.retiredWindows & ~reincarnated, absentRetiredWindows);
}

export function windowIsDeclared(lifecycle: PresentationLifecycle, window: number, open: boolean): boolean {
  if (!validEncodedPresentation(lifecycle)) return false;
  if (!open) return false;
  return (lifecycle.retiredWindows & windowBit(window)) === 0;
}

function visibleOwned(surface: OwnedPresentation): number {
  return surface.phase === "retired" ? -1 : surface.owner;
}

function surfaceContinuation(surface: PresentationSurface): number {
  if (surface.kind === "navigator") return surface.continuation;
  if (surface.kind === "settings") return surface.continuation;
  if (surface.kind === "host") return surface.continuation;
  if (surface.kind === "directory") return surface.continuation;
  if (surface.kind === "rename") return surface.continuation;
  return 0;
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
  if (!validEncodedPresentation(lifecycle)) lifecycle = initialPresentation();
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
