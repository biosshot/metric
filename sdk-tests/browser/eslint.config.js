import js from "@eslint/js";

export default [
  {
    ignores: ["dist/**", "node_modules/**"],
  },
  js.configs.recommended,
  {
    files: ["**/*.mjs", "**/*.js"],
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      globals: {
        Blob: "readonly",
        clearTimeout: "readonly",
        DecompressionStream: "readonly",
        document: "readonly",
        Event: "readonly",
        fetch: "readonly",
        Response: "readonly",
        setTimeout: "readonly",
        TextDecoder: "readonly",
        URLSearchParams: "readonly",
        window: "readonly",
      },
    },
  },
];
