import io.sentry.Sentry;
import io.sentry.protocol.SentryId;
import java.util.Timer;
import java.util.TimerTask;

public final class MetricSdkCompatibility {
  private MetricSdkCompatibility() {}

  public static void main(String[] args) {
    if (args.length != 1) {
      System.err.println("usage: MetricSdkCompatibility <dsn>");
      System.exit(2);
    }
    Timer watchdog = new Timer("metric-java-sdk-watchdog", true);
    watchdog.schedule(
        new TimerTask() {
          @Override
          public void run() {
            System.err.println("real Java SDK sender exceeded its 15 second process deadline");
            System.exit(124);
          }
        },
        15_000L);
    try {
      Sentry.init(
          options -> {
            options.setDsn(args[0]);
            options.setEnvironment("sdk-compatibility");
            options.setRelease("metric-java-sdk-test@1.0.0");
            options.setTracesSampleRate(0.0);
            options.setEnableAutoSessionTracking(false);
            options.setEnableShutdownHook(false);
            options.setSendDefaultPii(false);
          });
      SentryId eventId =
          Sentry.captureException(
              new MetricJavaSdkCompatibilityException(
                  "Metric real Java SDK compatibility event"));
      Sentry.flush(8_000L);
      Sentry.close();
      System.out.printf("{\"event_id\":\"%s\",\"flushed\":true}%n", eventId);
    } finally {
      watchdog.cancel();
    }
  }

  private static final class MetricJavaSdkCompatibilityException extends RuntimeException {
    private MetricJavaSdkCompatibilityException(String message) {
      super(message);
    }
  }
}
