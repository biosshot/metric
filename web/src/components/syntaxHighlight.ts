export type SyntaxTokenType = 'comment' | 'keyword' | 'number' | 'plain' | 'string';

export interface SyntaxToken {
  text: string;
  type: SyntaxTokenType;
}

const keywords = new Set([
  'const',
  'let',
  'var',
  'import',
  'from',
  'new',
  'true',
  'false',
  'null',
  'class',
  'public',
  'private',
  'static',
  'void',
  'string',
  'using',
  'def',
  'None',
  'package',
  'func',
  'function',
  'return',
  'throw',
  'try',
  'catch',
  'if',
  'else',
  'await',
  'async',
  'use',
  'mut',
]);

export function tokenizeSyntaxLine(line: string): SyntaxToken[] {
  const tokens: SyntaxToken[] = [];
  const pattern =
    /("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\/\/.*$|#.*$|\b\d+(?:\.\d+)?\b|\b[A-Za-z_][A-Za-z0-9_]*\b)/g;
  let cursor = 0;

  for (const match of line.matchAll(pattern)) {
    const index = match.index ?? 0;
    if (index > cursor) tokens.push({ text: line.slice(cursor, index), type: 'plain' });
    const text = match[0];
    let type: SyntaxTokenType = 'plain';
    if (text.startsWith('//') || text.startsWith('#')) type = 'comment';
    else if (text.startsWith('"') || text.startsWith("'")) type = 'string';
    else if (/^\d/.test(text)) type = 'number';
    else if (keywords.has(text)) type = 'keyword';
    tokens.push({ text, type });
    cursor = index + text.length;
  }

  if (cursor < line.length) tokens.push({ text: line.slice(cursor), type: 'plain' });
  return tokens;
}
