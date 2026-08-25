#!/bin/sh
printf 'HTTP_PROXY=%s\n' "${HTTP_PROXY-}"
printf 'HTTPS_PROXY=%s\n' "${HTTPS_PROXY-}"
printf 'ALL_PROXY=%s\n' "${ALL_PROXY-}"
printf 'http_proxy=%s\n' "${http_proxy-}"
printf 'https_proxy=%s\n' "${https_proxy-}"
printf 'all_proxy=%s\n' "${all_proxy-}"
printf 'XU_UPSTREAM_PROXY_MODE=%s\n' "${XU_UPSTREAM_PROXY_MODE-}"
printf 'NO_PROXY=%s\n' "${NO_PROXY-}"
printf '1.17.19\n'
