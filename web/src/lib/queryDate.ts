const localDatePattern =
  /(^|[\s(!])date:(>=|<=|>|<|=)?(?:"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2})"|(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}))(?=$|[\s)])/g;

function localTimestamp(value: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value);
  if (!match) return null;
  const year = Number(match[1] ?? Number.NaN);
  const month = Number(match[2] ?? Number.NaN);
  const day = Number(match[3] ?? Number.NaN);
  const hour = Number(match[4] ?? Number.NaN);
  const minute = Number(match[5] ?? Number.NaN);
  if (![year, month, day, hour, minute].every(Number.isInteger)) return null;
  const date = new Date(year, month - 1, day, hour, minute, 0, 0);
  if (
    date.getFullYear() !== year ||
    date.getMonth() !== month - 1 ||
    date.getDate() !== day ||
    date.getHours() !== hour ||
    date.getMinutes() !== minute
  ) {
    return null;
  }
  return date.getTime();
}

function insideQuotedValue(query: string, index: number): boolean {
  let quoted = false;
  let escaped = false;
  for (let position = 0; position < index; position += 1) {
    const character = query[position];
    if (escaped) escaped = false;
    else if (character === '\\') escaped = true;
    else if (character === '"') quoted = !quoted;
  }
  return quoted;
}

export function queryForBackend(query: string): string {
  return query.replace(
    localDatePattern,
    (predicate, prefix: string, operator: string | undefined, quoted, plain, offset: number) => {
      if (insideQuotedValue(query, offset + prefix.length)) return predicate;
      const timestamp = localTimestamp(String(quoted ?? plain));
      return timestamp === null ? predicate : `${prefix}timestamp:${operator ?? ''}${timestamp}`;
    },
  );
}
