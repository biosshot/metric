export function metricSourceMapFailure() {
  const operation = "symbolication-demo";
  const value = 42;

  // Keep the error away from the file boundaries to exercise both contexts.
  // The uploaded source must match the exact minified build.
  if (!operation || value !== 42) return;

  // This is the original source line highlighted in the event details.
  throw new Error("Metric real source map contract");

  // Source context is text around the location, including unreachable lines.
  // These comments deliberately exercise the post-context boundary.
  // The test must display the original source rather than the minified bundle.
}

export function metricSourceMapParse() {
  const request = "demo-request";

  // A real caller must survive source-map processing and retain its context.
  if (request) metricSourceMapFailure();

  return request;
}

export function metricSourceMapHandle() {
  const enabled = true;

  // The outer application frame is visible beneath the failing frame.
  if (enabled) return metricSourceMapParse();

  return null;
}
