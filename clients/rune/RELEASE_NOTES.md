**Phux Rune 0.1.0-alpha.18**: every machine's agents in the galaxy and the reef.

- **Other machines show their agents.** While the galaxy or the reef is open,
  Rune watches every other machine it can reach directly, read-only, and
  draws their agents live: stars round each planet, fish in each reef, in
  their real states. Before, other machines were empty planets and fishless
  reefs, and only once you had expanded them in the sidebar.
- **Links cross machines.** Agents in the same project on different machines
  are joined in the galaxy and school together in the reef.
- **Open an agent anywhere.** Clicking another machine's agent moves the
  window to that machine and shows its terminal. `N need you` cycles through
  waiting agents on every machine.
- The watches close when both views do. Machines reachable only over ssh are
  left out, as in the sidebar.
