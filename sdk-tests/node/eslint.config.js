import js from '@eslint/js';

export default [
  {
    ignores: ['node_modules/**'],
  },
  js.configs.recommended,
  {
    files: ['**/*.mjs', '**/*.js'],
    languageOptions: {
      ecmaVersion: 'latest',
      sourceType: 'module',
      globals: {
        Buffer: 'readonly',
        clearTimeout: 'readonly',
        setTimeout: 'readonly',
      },
    },
  },
];
