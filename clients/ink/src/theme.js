// One place for the palette and the small pieces of chrome, so the look is
// consistent and easy to retune. Truecolor hex works in modern terminals.

export const theme = {
  you: '#38bdf8', // sky — the human
  agent: '#e5e7eb', // near-white — the assistant
  accent: '#a78bfa', // violet — markers, the caret, active chrome
  tool: '#6b7280', // gray — tool activity, secondary text
  error: '#f87171', // red
  notice: '#fbbf24', // amber — the runtime asking for the human (trust cards)
  dim: '#4b5563', // very quiet — borders, placeholders
};

// The marker that precedes each speaker's lines.
export const marker = {
  you: '›',
  agent: '✦',
  tool: '⚙',
  error: '✕',
  notice: '⚠',
};
