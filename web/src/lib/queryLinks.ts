import type { RouteLocationRaw } from 'vue-router';

export type QueryFilter = readonly [field: string, value: string];

export function quoteQueryValue(value: string): string {
  return `"${value.replaceAll('\\', '\\\\').replaceAll('"', '\\"')}"`;
}

export function queryExpression(filters: readonly QueryFilter[]): string {
  return filters.map(([field, value]) => `${field}:${quoteQueryValue(value)}`).join(' AND ');
}

export function queryLinks(path: string, filters: readonly QueryFilter[]): RouteLocationRaw {
  return { path, query: { q: queryExpression(filters) } };
}

export function queryLink(path: string, field: string, value: string): RouteLocationRaw {
  return queryLinks(path, [[field, value]]);
}
