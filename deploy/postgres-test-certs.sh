#!/bin/sh
# Ephemeral synthetic TLS credentials for the isolated Docker test fixture only.
set -eu
umask 077
cd /certs
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=Metric-Test-CA \
  -keyout ca.key -out ca.crt >/dev/null 2>&1
openssl req -new -newkey rsa:2048 -nodes -subj /CN=postgres-tls \
  -keyout server.key -out server.csr >/dev/null 2>&1
printf '%s\n' 'subjectAltName=DNS:postgres-tls,IP:127.0.0.1' 'extendedKeyUsage=serverAuth' > server.ext
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 \
  -extfile server.ext -out server.crt >/dev/null 2>&1
openssl req -new -newkey rsa:2048 -nodes -subj /CN=metric-test-client \
  -keyout client.key -out client.csr >/dev/null 2>&1
printf '%s\n' 'extendedKeyUsage=clientAuth' > client.ext
openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 \
  -extfile client.ext -out client.crt >/dev/null 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=Metric-Wrong-CA \
  -keyout wrong-ca.key -out wrong-ca.crt >/dev/null 2>&1
chown 999:999 server.key
chmod 600 server.key
chmod 644 ca.crt server.crt client.crt wrong-ca.crt
