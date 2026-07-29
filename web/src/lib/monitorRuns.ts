import type { MonitorRun } from '../api/types';

const DEFAULT_TIMELINE_POINTS = 512;

export function newestMonitorRunsFirst(runs: readonly MonitorRun[]): MonitorRun[] {
  return [...runs].sort((left, right) => {
    const timeDifference = runTime(right) - runTime(left);
    return timeDifference || right.id.localeCompare(left.id);
  });
}

export function sampleMonitorTimeline(
  runs: readonly MonitorRun[],
  maximumPoints = DEFAULT_TIMELINE_POINTS,
): MonitorRun[] {
  const ordered = newestMonitorRunsFirst(runs);
  if (maximumPoints < 3) return ordered.slice(0, Math.max(0, maximumPoints));
  if (ordered.length <= maximumPoints) return ordered;

  const sampled = [ordered[0]];
  const interiorLength = ordered.length - 2;
  const bucketCount = Math.max(1, Math.floor((maximumPoints - 2) / 2));

  for (let bucket = 0; bucket < bucketCount; bucket += 1) {
    const rangeStart = 1 + Math.floor((bucket * interiorLength) / bucketCount);
    const rangeEnd = 1 + Math.floor(((bucket + 1) * interiorLength) / bucketCount);
    let minimumIndex = rangeStart;
    let maximumIndex = rangeStart;

    for (let index = rangeStart + 1; index < rangeEnd; index += 1) {
      const duration = ordered[index].duration_ms ?? 0;
      if (duration < (ordered[minimumIndex].duration_ms ?? 0)) minimumIndex = index;
      if (duration > (ordered[maximumIndex].duration_ms ?? 0)) maximumIndex = index;
    }

    sampled.push(ordered[Math.min(minimumIndex, maximumIndex)]);
    if (minimumIndex !== maximumIndex) {
      sampled.push(ordered[Math.max(minimumIndex, maximumIndex)]);
    }
  }

  sampled.push(ordered[ordered.length - 1]);
  return sampled;
}

function runTime(run: MonitorRun): number {
  return new Date(run.started_at).getTime();
}
