ENV["BUNDLE_GEMFILE"] = File.expand_path("Gemfile", __dir__)
ENV["BUNDLE_PATH"] = File.expand_path(".deps", __dir__)
ENV["BUNDLE_FROZEN"] = "true"
require "bundler/setup"
require "sentry-ruby"
require "json"
require "logger"

abort "Metric DSN argument is required" unless ARGV.length == 1
watchdog = Thread.new do
  sleep 20
  warn "Real Ruby SDK sender exceeded its process deadline"
  exit! 2
end

begin
  Sentry.init do |config|
    config.dsn = ARGV[0]
    config.environment = "sdk-compatibility"
    config.release = "metric-ruby-sdk-test@1.0.0"
    config.send_default_pii = false
    config.traces_sample_rate = 0.0
    config.background_worker_threads = 2
    config.transport.open_timeout = 2
    config.transport.timeout = 5
    config.sdk_logger = Logger.new($stderr)
  end
  Sentry.configure_scope { |scope| scope.set_tags("metric.sdk_test" => "ruby") }
  event = Sentry.capture_exception(RuntimeError.new("Metric real Ruby SDK compatibility event"))
  raise "Ruby SDK did not capture the exception" unless event
  event_id = event.event_id
  Sentry.close
  puts JSON.generate(event_id: event_id, flushed: true, sdk_version: Sentry::VERSION, runtime: RUBY_VERSION)
ensure
  watchdog.kill
end
