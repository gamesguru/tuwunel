#!/bin/bash
# Provision-then-exec wrapper for the Complement testee.
#
# Complement generates a fresh CA per run and copies it into every homeserver
# container at /complement/ca/{ca.crt,ca.key}. Each homeserver is expected to
# present a federation certificate signed by that CA so peers which validate
# certificates (e.g. Synapse) accept the connection. We sign one here for
# $SERVER_NAME and overwrite the baked self-signed certificate the image ships,
# then exec the server passed as our arguments.
#
# When the CA is absent (running outside Complement) the baked certificate is
# left in place, so certificate provisioning is a no-op in that case.
#
# After provisioning, the server is handed to sched_wrap.sh, which applies an
# optional scheduling prefix from the environment (sched_policy and sched_prio
# for chrt, sched_nice for nice, sched_ionice for ionice) before exec'ing it.
# Each knob is opt-in, so with none set that is a plain exec. The realtime
# policies and negative niceness need CAP_SYS_NICE, which the Complement runner
# adds to the testee container. Provisioning runs first, so only the server and
# the tree it spawns are scheduled.
set -eo pipefail

ca_crt="/complement/ca/ca.crt"
ca_key="/complement/ca/ca.key"
tls_crt="/complement/certificate.crt"
tls_key="/complement/private_key.pem"
tls_conf="/complement/server.tls.conf"
tls_csr="/complement/server.tls.csr"

provision() {
	test -f "$ca_crt" && test -f "$ca_key" && test -n "${SERVER_NAME:-}" || return 1

	printf '.include /etc/ssl/openssl.cnf\n\n[SAN]\nsubjectAltName=DNS:%s\n' \
		"$SERVER_NAME" > "$tls_conf"

	openssl genrsa -out "$tls_key" 2048
	openssl req -new -config "$tls_conf" -key "$tls_key" -out "$tls_csr" \
		-subj "/CN=$SERVER_NAME" -reqexts SAN
	openssl x509 -req -in "$tls_csr" \
		-CA "$ca_crt" -CAkey "$ca_key" -set_serial 1 \
		-out "$tls_crt" -extfile "$tls_conf" -extensions SAN
}

if provision 2>/tmp/provision_complement.log; then
	echo "complement: federation certificate signed by run CA for ${SERVER_NAME}"
else
	echo "complement: no run CA, keeping baked certificate" >&2
fi

# Hand off to the scheduling wrapper, which applies any sched_policy, sched_nice
# or sched_ionice prefix from the environment and execs the provisioned server.
exec sched_wrap.sh "$@"
