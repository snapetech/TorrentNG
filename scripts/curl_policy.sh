# shellcheck shell=bash
# Shared policy for certification HTTP calls. Source this file after defining ROOT.

_tng_curl_header_is_sensitive() {
  local candidate="$1" line header_name header_file
  if [[ "$candidate" == @* ]]; then
    if [[ "$candidate" == "@-" ]]; then
      return 0
    fi
    header_file="${candidate#@}"
    # Treat special files and symlinks as opaque sensitive inputs so callers
    # fail closed instead of blocking on a FIFO or classifying it as harmless.
    [[ -f "$header_file" && ! -L "$header_file" && -r "$header_file" ]] || return 0
    while IFS= read -r line || [[ -n "$line" ]]; do
      header_name="${line%%:*}"
      header_name="${header_name//[[:space:]]/}"
      case "${header_name,,}" in
        *authorization*|*cookie*|*auth*|*token*|*key*|*secret*|*pass*|*credential*|*session*|*csrf*|*signature*|referer) return 0 ;;
      esac
    done < "$header_file"
    return 1
  fi
  header_name="${candidate%%:*}"
  header_name="${header_name//[[:space:]]/}"
  case "${header_name,,}" in
    *authorization*|*cookie*|*auth*|*token*|*key*|*secret*|*pass*|*credential*|*session*|*csrf*|*signature*|referer) return 0 ;;
    *) return 1 ;;
  esac
}

_tng_curl_header_has_proxy_auth() {
  local candidate="$1" line header_name header_file
  if [[ "$candidate" == @* ]]; then
    [[ "$candidate" != "@-" ]] || return 1
    header_file="${candidate#@}"
    [[ -f "$header_file" && ! -L "$header_file" && -r "$header_file" ]] || return 1
    while IFS= read -r line || [[ -n "$line" ]]; do
      header_name="${line%%:*}"
      header_name="${header_name//[[:space:]]/}"
      [[ "${header_name,,}" == "proxy-authorization" ]] && return 0
    done < "$header_file"
    return 1
  fi
  header_name="${candidate%%:*}"
  header_name="${header_name//[[:space:]]/}"
  [[ "${header_name,,}" == "proxy-authorization" ]]
}

_tng_curl_url_has_userinfo() {
  local candidate="$1"
  [[ "$candidate" == --url=* ]] && candidate="${candidate#*=}"
  candidate="${candidate,,}"
  [[ "$candidate" =~ ^([a-z][a-z0-9+.-]*://)?[^/?#]*@ ]]
}

_tng_curl_decode_query_name() {
  local input="$1" decoded="" character hex
  local index=0 layer=0
  while ((layer < 8)); do
    decoded=""
    index=0
    while ((index < ${#input})); do
      character="${input:index:1}"
      if [[ "$character" == "%" && $((index + 2)) -lt ${#input} ]]; then
        hex="${input:index+1:2}"
        if [[ "$hex" =~ ^[[:xdigit:]]{2}$ ]]; then
          printf -v character '%b' "\\x$hex"
          index=$((index + 2))
        fi
      fi
      decoded+="$character"
      index=$((index + 1))
    done
    [[ "$decoded" != "$input" ]] || break
    input="$decoded"
    layer=$((layer + 1))
  done
  printf '%s' "${input,,}"
}

_tng_curl_query_name_is_sensitive() {
  local name LC_ALL=C
  name="$(_tng_curl_decode_query_name "$1")"
  name="${name//[^a-z0-9]/}"
  case "$name" in
    *apikey*|*passkey*|pass|*passwd*|*password*|*passphrase*|*torrentpass*|*trackerpass*|pwd|pid|uk|sig|*token*|*auth*|*secret*|*session*|*signature*|*credential*|*bearer*|*cookie*|*key*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

_tng_curl_data_query_sensitivity() {
  local option="$1" value="$2" data field name
  if [[ "$option" == "--data-urlencode" ]]; then
    # curl treats both @file and name@file as file-backed inputs. In an
    # authenticated GET, neither can be inspected for credential-like query
    # values, even when the field name itself looks harmless.
    [[ "$value" != @* && ! ( "$value" == *@* && "$value" != *=* ) ]] || return 2
    if [[ "$value" == =* ]]; then
      name="${value#=}"
    elif [[ "$value" == *=* || "$value" == *@* ]]; then
      name="${value%%[=@]*}"
    else
      name="$value"
    fi
    _tng_curl_query_name_is_sensitive "$name"
    return $?
  fi

  if [[ "$option" != "--data-raw" && "$value" == @* ]]; then
    return 2
  fi
  data="${value//;/&}"
  while :; do
    if [[ "$data" == *'&'* ]]; then
      field="${data%%&*}"
      data="${data#*&}"
    else
      field="$data"
      data=""
    fi
    name="${field%%=*}"
    _tng_curl_query_name_is_sensitive "$name" && return 0
    [[ -n "$data" ]] || break
  done
  return 1
}

_tng_curl_url_has_sensitive_query() {
  local candidate="$1" query field name
  [[ "$candidate" == --url=* ]] && candidate="${candidate#*=}"
  [[ "$candidate" != *#* ]] || return 1
  [[ "$candidate" == *\?* ]] || return 1
  query="${candidate#*\?}"
  query="${query//;/&}"
  while :; do
    if [[ "$query" == *'&'* ]]; then
      field="${query%%&*}"
      query="${query#*&}"
    else
      field="$query"
      query=""
    fi
    name="${field%%=*}"
    _tng_curl_query_name_is_sensitive "$name" && return 0
    [[ -n "$query" ]] || break
  done
  return 1
}

_tng_curl_url_has_fragment() {
  local candidate="$1"
  [[ "$candidate" == --url=* ]] && candidate="${candidate#*=}"
  [[ "$candidate" == *#* ]]
}

_tng_curl_write_out_is_safe() {
  local format="$1"
  [[ "$format" != @* ]] || return 1
  while [[ -n "$format" ]]; do
    case "$format" in
      '%{http_code}'*) format="${format:12}" ;;
      '%{time_total}'*) format="${format:13}" ;;
      '%%'*|'\n'*|'\r'*|'\t'*) format="${format:2}" ;;
      *) return 1 ;;
    esac
  done
  return 0
}

tng_write_qbit_login_body() {
  local token="$1" output="$2"
  TNG_AUTH_TOKEN="$token" python3 -c \
    'import os, sys, urllib.parse; sys.stdout.write(urllib.parse.urlencode({"username": os.environ["TNG_AUTH_TOKEN"], "password": os.environ["TNG_AUTH_TOKEN"]}))' \
    > "$output"
}

_tng_curl_write_private_file() {
  local contents="$1" private_path
  private_path="$(mktemp "${TMPDIR:-/tmp}/tng-curl-data.XXXXXX")" || return 1
  if ! chmod 600 -- "$private_path" || ! printf '%s' "$contents" > "$private_path"; then
    rm -f -- "$private_path"
    return 1
  fi
  printf '%s' "$private_path"
}

_tng_curl_cleanup_files() {
  local private_path
  for private_path in "$@"; do
    rm -f -- "$private_path"
  done
}

_tng_curl_prepare_cookie_jar() {
  local cookie_jar="$1"
  case "$cookie_jar" in
    ""|-|/dev/stdout|/dev/stderr|/dev/fd|/dev/fd/*|/proc/self/fd/*|NUL)
      echo "curl policy: cookie jars must not target stdout or a device" >&2
      return 1
      ;;
  esac
  if [[ -L "$cookie_jar" || ! -f "$cookie_jar" || ! -O "$cookie_jar" || ! -r "$cookie_jar" || ! -w "$cookie_jar" ]]; then
    echo "curl policy: cookie jar must be an existing owned regular file" >&2
    return 1
  fi
  chmod 600 -- "$cookie_jar"
}

_tng_curl_prepare_cookie_file() {
  local cookie_file="$1"
  case "$cookie_file" in
    ""|-)
      return 0
      ;;
    /dev/stdin|/dev/stdout|/dev/stderr|/dev/fd|/dev/fd/*|/proc/self/fd/*|NUL)
      echo "curl policy: cookie input must not target a device or descriptor path" >&2
      return 1
      ;;
  esac
  if [[ -L "$cookie_file" || ! -f "$cookie_file" || ! -O "$cookie_file" || ! -r "$cookie_file" ]]; then
    echo "curl policy: cookie input must be an existing owned regular file" >&2
    return 1
  fi
  chmod 600 -- "$cookie_file"
}

_tng_curl_prepare_netrc_file() {
  local netrc_file="$1"
  if [[ -L "$netrc_file" || ! -f "$netrc_file" || ! -O "$netrc_file" || ! -r "$netrc_file" ]]; then
    echo "curl policy: netrc credentials must come from an existing owned regular file" >&2
    return 1
  fi
  chmod 600 -- "$netrc_file"
}

_tng_curl_prepare_default_netrc() {
  local optional="$1" netrc_override="${NETRC:-}" home_netrc="" found=0
  if [[ -n "$netrc_override" ]]; then
    if [[ -e "$netrc_override" || -L "$netrc_override" ]]; then
      if ! _tng_curl_prepare_netrc_file "$netrc_override"; then
        return 1
      fi
      found=1
    elif [[ "$optional" == "0" ]]; then
      echo "curl policy: required NETRC file does not exist" >&2
      return 1
    fi
  fi

  if [[ -n "${HOME:-}" ]]; then
    home_netrc="${HOME%/}/.netrc"
    if [[ -e "$home_netrc" || -L "$home_netrc" ]]; then
      if ! _tng_curl_prepare_netrc_file "$home_netrc"; then
        return 1
      fi
      found=1
    elif [[ -z "$netrc_override" && "$optional" == "0" ]]; then
      echo "curl policy: required home netrc file does not exist" >&2
      return 1
    fi
  fi

  if [[ -z "$netrc_override" && -z "$home_netrc" ]]; then
    echo "curl policy: cannot resolve the default netrc path without HOME or NETRC" >&2
    return 1
  fi
  if [[ "$optional" == "0" && "$found" == "0" ]]; then
    echo "curl policy: required netrc file does not exist" >&2
    return 1
  fi
}

_tng_curl_copy_private_header_file() {
  local source="$1" private_path
  if [[ ! -f "$source" || -L "$source" || ! -r "$source" ]]; then
    echo "curl policy: sensitive header file is not a readable regular file" >&2
    return 2
  fi
  private_path="$(mktemp "${TMPDIR:-/tmp}/tng-curl-headers.XXXXXX")" || return 1
  if ! cat -- "$source" > "$private_path" || ! chmod 600 -- "$private_path"; then
    rm -f -- "$private_path"
    return 1
  fi
  printf '%s' "$private_path"
}

curl() {
  local -a forwarded=() private_headers=() private_files=()
  local header header_file="" private_header_copy status=0 has_auth=0 has_redirect=0 has_diagnostic_output=0 has_unsafe_writeout=0 url_count=0 url_argument_count=0
  local has_get=0 has_sensitive_query_data=0 has_uninspectable_query_data=0 query_status
  local data_option data_value data_content data_name data_file
  local form_value form_name form_content form_file
  local credential encoded_cookie url url_argument write_out_format netrc_file netrc_optional

  while (($#)); do
    case "$1" in
      -H|--header)
        if (($# < 2)); then
          echo "curl policy: missing header argument" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        header="$2"
        if _tng_curl_header_has_proxy_auth "$header"; then
          echo "curl policy: Proxy-Authorization headers are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_header_is_sensitive "$header"; then
          has_auth=1
          if [[ "$header" == "@-" ]]; then
            echo "curl policy: sensitive headers cannot be read from stdin" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          elif [[ "$header" == @* ]]; then
            if ! private_header_copy="$(_tng_curl_copy_private_header_file "${header#@}")"; then
              _tng_curl_cleanup_files "${private_files[@]}"
              return 2
            fi
            private_files+=("$private_header_copy")
            forwarded+=("$1" "@$private_header_copy")
          else
            if [[ "$header" == *$'\n'* || "$header" == *$'\r'* ]]; then
              echo "curl policy: authentication headers cannot contain line breaks" >&2
              _tng_curl_cleanup_files "${private_files[@]}"
              return 2
            fi
            private_headers+=("$header")
          fi
        else
          forwarded+=("$1" "$header")
        fi
        shift 2
        ;;
      --header=*)
        header="${1#*=}"
        if _tng_curl_header_has_proxy_auth "$header"; then
          echo "curl policy: Proxy-Authorization headers are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_header_is_sensitive "$header"; then
          has_auth=1
          if [[ "$header" == "@-" ]]; then
            echo "curl policy: sensitive headers cannot be read from stdin" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          elif [[ "$header" == @* ]]; then
            if ! private_header_copy="$(_tng_curl_copy_private_header_file "${header#@}")"; then
              _tng_curl_cleanup_files "${private_files[@]}"
              return 2
            fi
            private_files+=("$private_header_copy")
            forwarded+=(--header "@$private_header_copy")
          else
            if [[ "$header" == *$'\n'* || "$header" == *$'\r'* ]]; then
              echo "curl policy: authentication headers cannot contain line breaks" >&2
              _tng_curl_cleanup_files "${private_files[@]}"
              return 2
            fi
            private_headers+=("$header")
          fi
        else
          forwarded+=("--header" "$header")
        fi
        shift
        ;;
      -H?*)
        header="${1:2}"
        if _tng_curl_header_has_proxy_auth "$header"; then
          echo "curl policy: Proxy-Authorization headers are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_header_is_sensitive "$header"; then
          has_auth=1
          if [[ "$header" == "@-" ]]; then
            echo "curl policy: sensitive headers cannot be read from stdin" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          elif [[ "$header" == @* ]]; then
            if ! private_header_copy="$(_tng_curl_copy_private_header_file "${header#@}")"; then
              _tng_curl_cleanup_files "${private_files[@]}"
              return 2
            fi
            private_files+=("$private_header_copy")
            forwarded+=(--header "@$private_header_copy")
          else
            if [[ "$header" == *$'\n'* || "$header" == *$'\r'* ]]; then
              echo "curl policy: authentication headers cannot contain line breaks" >&2
              _tng_curl_cleanup_files "${private_files[@]}"
              return 2
            fi
            private_headers+=("$header")
          fi
        else
          forwarded+=(--header "$header")
        fi
        shift
        ;;
      -e|--referer)
        if (($# < 2)); then
          echo "curl policy: missing Referer URL" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        credential="$2"
        if [[ "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: Referer URL cannot contain line breaks" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if [[ "$credential" == *';auto' ]]; then
          echo "curl policy: automatic Referer redirects are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Referer: $credential")
        has_auth=1
        shift 2
        ;;
      --referer=*)
        credential="${1#*=}"
        if [[ "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: Referer URL cannot contain line breaks" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if [[ "$credential" == *';auto' ]]; then
          echo "curl policy: automatic Referer redirects are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Referer: $credential")
        has_auth=1
        shift
        ;;
      -e?*)
        credential="${1:2}"
        if [[ "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: Referer URL cannot contain line breaks" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if [[ "$credential" == *';auto' ]]; then
          echo "curl policy: automatic Referer redirects are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Referer: $credential")
        has_auth=1
        shift
        ;;
      -d|--data|--data-ascii|--data-binary|--data-raw|--data-urlencode|--json)
        if (($# < 2)); then
          echo "curl policy: missing request body argument" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        data_option="$1"
        data_value="$2"
        has_auth=1
        if _tng_curl_data_query_sensitivity "$data_option" "$data_value"; then
          has_sensitive_query_data=1
        else
          query_status=$?
          if ((query_status == 2)); then
            has_uninspectable_query_data=1
          fi
        fi
        if [[ "$data_option" != "--data-raw" && "$data_option" != "--data-urlencode" && "$data_value" == @* ]]; then
          forwarded+=("$data_option" "$data_value")
        elif [[ "$data_option" == "--data-urlencode" && "$data_value" == @* ]]; then
          forwarded+=("$data_option" "$data_value")
        elif [[ "$data_option" == "--data-urlencode" && "$data_value" == *@* && "$data_value" != *=* ]]; then
          forwarded+=("$data_option" "$data_value")
        else
          data_content="$data_value"
          if [[ "$data_option" == "--data-urlencode" ]]; then
            if [[ "$data_value" == =* ]]; then
              data_content="${data_value#=}"
              data_name=""
            elif [[ "$data_value" == *=* ]]; then
              data_name="${data_value%%=*}"
              data_content="${data_value#*=}"
            else
              data_name=""
            fi
          fi
          if ! data_file="$(_tng_curl_write_private_file "$data_content")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$data_file")
          if [[ "$data_option" == "--json" ]]; then
            forwarded+=(--json "@$data_file")
          elif [[ "$data_option" == "--data-raw" ]]; then
            forwarded+=(--data-binary "@$data_file")
          elif [[ "$data_option" == "--data-urlencode" && -n "$data_name" ]]; then
            forwarded+=(--data-urlencode "$data_name@$data_file")
          elif [[ "$data_option" == "--data-urlencode" ]]; then
            forwarded+=(--data-urlencode "@$data_file")
          else
            forwarded+=("$data_option" "@$data_file")
          fi
        fi
        shift 2
        ;;
      --user|-u)
        if (($# < 2)); then
          echo "curl policy: missing basic-auth credentials" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        credential="$2"
        if [[ "$credential" != *:* || "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: basic-auth credentials must be a single-line user:password pair" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Authorization: Basic $(printf '%s' "$credential" | command base64 | tr -d '\r\n')")
        has_auth=1
        shift 2
        ;;
      --user=*)
        credential="${1#*=}"
        if [[ "$credential" != *:* || "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: basic-auth credentials must be a single-line user:password pair" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Authorization: Basic $(printf '%s' "$credential" | command base64 | tr -d '\r\n')")
        has_auth=1
        shift
        ;;
      -u?*)
        credential="${1:2}"
        if [[ "$credential" != *:* || "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: basic-auth credentials must be a single-line user:password pair" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Authorization: Basic $(printf '%s' "$credential" | command base64 | tr -d '\r\n')")
        has_auth=1
        shift
        ;;
      --oauth2-bearer)
        if (($# < 2)); then
          echo "curl policy: missing OAuth bearer token" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        credential="$2"
        if [[ -z "$credential" || "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: OAuth bearer token is empty or contains line breaks" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Authorization: Bearer $credential")
        has_auth=1
        shift 2
        ;;
      --oauth2-bearer=*)
        credential="${1#*=}"
        if [[ -z "$credential" || "$credential" == *$'\n'* || "$credential" == *$'\r'* ]]; then
          echo "curl policy: OAuth bearer token is empty or contains line breaks" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        private_headers+=("Authorization: Bearer $credential")
        has_auth=1
        shift
        ;;
      --cookie|-b)
        if (($# < 2)); then
          echo "curl policy: missing cookie argument" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        encoded_cookie="$2"
        if [[ -z "$encoded_cookie" || "$encoded_cookie" != *=* ]]; then
          if [[ -n "$encoded_cookie" && "$encoded_cookie" != "-" ]] && ! _tng_curl_prepare_cookie_file "$encoded_cookie"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
          forwarded+=("$1" "$encoded_cookie")
          [[ -z "$encoded_cookie" ]] || has_auth=1
        else
          if [[ "$encoded_cookie" == *$'\n'* || "$encoded_cookie" == *$'\r'* ]]; then
            echo "curl policy: inline cookies cannot contain line breaks" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
          private_headers+=("Cookie: $encoded_cookie")
          has_auth=1
        fi
        shift 2
        ;;
      --cookie=*)
        encoded_cookie="${1#*=}"
        if [[ -z "$encoded_cookie" || "$encoded_cookie" != *=* ]]; then
          if [[ -n "$encoded_cookie" && "$encoded_cookie" != "-" ]] && ! _tng_curl_prepare_cookie_file "$encoded_cookie"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
          forwarded+=(--cookie "$encoded_cookie")
          [[ -z "$encoded_cookie" ]] || has_auth=1
        else
          if [[ "$encoded_cookie" == *$'\n'* || "$encoded_cookie" == *$'\r'* ]]; then
            echo "curl policy: inline cookies cannot contain line breaks" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
          private_headers+=("Cookie: $encoded_cookie")
          has_auth=1
        fi
        shift
        ;;
      -c|--cookie-jar)
        if (($# < 2)); then
          echo "curl policy: missing cookie-jar destination" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if ! _tng_curl_prepare_cookie_jar "$2"; then
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        forwarded+=("$1" "$2")
        shift 2
        ;;
      -c?*)
        cookie_jar="${1:2}"
        if ! _tng_curl_prepare_cookie_jar "$cookie_jar"; then
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        forwarded+=(--cookie-jar "$cookie_jar")
        shift
        ;;
      --cookie-jar=*)
        cookie_jar="${1#*=}"
        if ! _tng_curl_prepare_cookie_jar "$cookie_jar"; then
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        forwarded+=(--cookie-jar "$cookie_jar")
        shift
        ;;
      -b?*)
        encoded_cookie="${1:2}"
        if [[ -z "$encoded_cookie" || "$encoded_cookie" != *=* ]]; then
          if [[ -n "$encoded_cookie" && "$encoded_cookie" != "-" ]] && ! _tng_curl_prepare_cookie_file "$encoded_cookie"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
          forwarded+=(--cookie "$encoded_cookie")
          [[ -z "$encoded_cookie" ]] || has_auth=1
        else
          if [[ "$encoded_cookie" == *$'\n'* || "$encoded_cookie" == *$'\r'* ]]; then
            echo "curl policy: inline cookies cannot contain line breaks" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
          private_headers+=("Cookie: $encoded_cookie")
          has_auth=1
        fi
        shift
        ;;
      --form|--form-string|-F)
        if (($# < 2)); then
          echo "curl policy: missing multipart form argument" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        form_value="$2"
        if [[ "$form_value" != *=* ]]; then
          echo "curl policy: multipart field must be name=value" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        form_name="${form_value%%=*}"
        form_content="${form_value#*=}"
        if [[ "$1" == "--form-string" ]]; then
          if ! form_file="$(_tng_curl_write_private_file "$form_content")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$form_file")
          forwarded+=(--form "$form_name=<$form_file")
        elif [[ "$form_content" == @* || "$form_content" == \<* ]]; then
          forwarded+=(--form "$form_value")
        elif [[ "$form_content" == *\;* ]]; then
          echo "curl policy: inline multipart parameters must use a file input or --form-string" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        else
          if ! form_file="$(_tng_curl_write_private_file "$form_content")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$form_file")
          forwarded+=(--form "$form_name=<$form_file")
        fi
        has_auth=1
        shift 2
        ;;
      --form=*|--form-string=*)
        form_value="${1#*=}"
        if [[ "$form_value" != *=* ]]; then
          echo "curl policy: multipart field must be name=value" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        form_name="${form_value%%=*}"
        form_content="${form_value#*=}"
        if [[ "$1" == --form-string=* || ( "$form_content" != @* && "$form_content" != \<* ) ]]; then
          if [[ "$1" == --form=* && "$form_content" == *\;* ]]; then
            echo "curl policy: inline multipart parameters must use a file input or --form-string" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
          if ! form_file="$(_tng_curl_write_private_file "$form_content")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$form_file")
          forwarded+=(--form "$form_name=<$form_file")
        else
          forwarded+=(--form "$form_value")
        fi
        has_auth=1
        shift
        ;;
      -F?*)
        form_value="${1:2}"
        if [[ "$form_value" != *=* ]]; then
          echo "curl policy: multipart field must be name=value" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        form_name="${form_value%%=*}"
        form_content="${form_value#*=}"
        if [[ "$form_content" == @* || "$form_content" == \<* ]]; then
          forwarded+=(--form "$form_value")
        elif [[ "$form_content" == *\;* ]]; then
          echo "curl policy: inline multipart parameters must use a file input or --form-string" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        else
          if ! form_file="$(_tng_curl_write_private_file "$form_content")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$form_file")
          forwarded+=(--form "$form_name=<$form_file")
        fi
        has_auth=1
        shift
        ;;
      --json=*)
        data_option=--json
        data_value="${1#*=}"
        has_auth=1
        if _tng_curl_data_query_sensitivity "$data_option" "$data_value"; then
          has_sensitive_query_data=1
        else
          query_status=$?
          if ((query_status == 2)); then
            has_uninspectable_query_data=1
          fi
        fi
        if [[ "$data_value" == @* ]]; then
          forwarded+=(--json "$data_value")
        else
          if ! data_file="$(_tng_curl_write_private_file "$data_value")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$data_file")
          forwarded+=(--json "@$data_file")
        fi
        shift
        ;;
      --data=*)
        data_option=--data
        data_value="${1#*=}"
        has_auth=1
        if _tng_curl_data_query_sensitivity "$data_option" "$data_value"; then
          has_sensitive_query_data=1
        else
          query_status=$?
          if ((query_status == 2)); then
            has_uninspectable_query_data=1
          fi
        fi
        if [[ "$data_value" == @* ]]; then
          forwarded+=(--data "$data_value")
        else
          if ! data_file="$(_tng_curl_write_private_file "$data_value")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$data_file")
          forwarded+=(--data "@$data_file")
        fi
        shift
        ;;
      --data-ascii=*|--data-binary=*)
        data_option="${1%%=*}"
        data_value="${1#*=}"
        has_auth=1
        if _tng_curl_data_query_sensitivity "$data_option" "$data_value"; then
          has_sensitive_query_data=1
        else
          query_status=$?
          if ((query_status == 2)); then
            has_uninspectable_query_data=1
          fi
        fi
        if [[ "$data_value" == @* ]]; then
          forwarded+=("$data_option" "$data_value")
        else
          if ! data_file="$(_tng_curl_write_private_file "$data_value")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$data_file")
          forwarded+=("$data_option" "@$data_file")
        fi
        shift
        ;;
      --data-raw=*|--data-urlencode=*)
        data_option="${1%%=*}"
        data_value="${1#*=}"
        has_auth=1
        if _tng_curl_data_query_sensitivity "$data_option" "$data_value"; then
          has_sensitive_query_data=1
        else
          query_status=$?
          if ((query_status == 2)); then
            has_uninspectable_query_data=1
          fi
        fi
        if [[ "$data_option" == "--data-urlencode" && ( "$data_value" == @* || ( "$data_value" == *@* && "$data_value" != *=* ) ) ]]; then
          forwarded+=("$data_option" "$data_value")
        else
          data_content="$data_value"
          if [[ "$data_option" == "--data-urlencode" ]]; then
            if [[ "$data_value" == =* ]]; then
              data_content="${data_value#=}"
              data_name=""
            elif [[ "$data_value" == *=* ]]; then
              data_name="${data_value%%=*}"
              data_content="${data_value#*=}"
            else
              data_name=""
            fi
          fi
          if ! data_file="$(_tng_curl_write_private_file "$data_content")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$data_file")
          if [[ "$data_option" == "--data-raw" ]]; then
            forwarded+=(--data-binary "@$data_file")
          elif [[ "$data_option" == "--data-urlencode" && -n "$data_name" ]]; then
            forwarded+=(--data-urlencode "$data_name@$data_file")
          elif [[ "$data_option" == "--data-urlencode" ]]; then
            forwarded+=(--data-urlencode "@$data_file")
          else
            forwarded+=("$data_option" "@$data_file")
          fi
        fi
        shift
        ;;
      -d?*)
        data_option=--data
        data_value="${1:2}"
        has_auth=1
        if _tng_curl_data_query_sensitivity "$data_option" "$data_value"; then
          has_sensitive_query_data=1
        else
          query_status=$?
          if ((query_status == 2)); then
            has_uninspectable_query_data=1
          fi
        fi
        if [[ "$data_value" == @* ]]; then
          forwarded+=(-d "$data_value")
        else
          if ! data_file="$(_tng_curl_write_private_file "$data_value")"; then
            _tng_curl_cleanup_files "${private_files[@]}"
            return 1
          fi
          private_files+=("$data_file")
          forwarded+=(-d "@$data_file")
        fi
        shift
        ;;
      --get|-G)
        has_get=1
        forwarded+=("$1")
        shift
        ;;
      --no-get)
        has_get=0
        forwarded+=("$1")
        shift
        ;;
      --config|-K|--config=*|-K?*)
        echo "curl policy: caller-supplied curl config is disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --libcurl|--libcurl=*)
        echo "curl policy: generated libcurl source can contain credentials and is disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --variable|--variable=*|--expand-*)
        echo "curl policy: curl variable expansion is disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --pass|--pass=*|--cert|-E|--cert=*|-E?*|--key|--key=*|--httpsig-key|--httpsig-key=*|--tlsuser|--tlsuser=*|--tlspassword|--tlspassword=*|--tlsauthtype|--tlsauthtype=*)
        echo "curl policy: command-line client credential material is disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --netrc|-n|--netrc-optional)
        netrc_optional=0
        [[ "$1" != "--netrc-optional" ]] || netrc_optional=1
        if ! _tng_curl_prepare_default_netrc "$netrc_optional"; then
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        has_auth=1
        forwarded+=("$1")
        shift
        ;;
      --netrc-file)
        if (($# < 2)); then
          echo "curl policy: missing netrc file argument" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        netrc_file="$2"
        if ! _tng_curl_prepare_netrc_file "$netrc_file"; then
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        has_auth=1
        forwarded+=("$1" "$netrc_file")
        shift 2
        ;;
      --netrc-file=*)
        netrc_file="${1#*=}"
        if ! _tng_curl_prepare_netrc_file "$netrc_file"; then
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        has_auth=1
        forwarded+=(--netrc-file "$netrc_file")
        shift
        ;;
      --aws-sigv4)
        if (($# < 2)); then
          echo "curl policy: missing AWS signature provider" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        has_auth=1
        forwarded+=("$1" "$2")
        shift 2
        ;;
      --aws-sigv4=*)
        has_auth=1
        forwarded+=(--aws-sigv4 "${1#*=}")
        shift
        ;;
      --url)
        if (($# < 2)); then
          echo "curl policy: missing URL argument" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        url="$2"
        if [[ "$2" == @* ]]; then
          echo "curl policy: URL lists from files are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_url_has_userinfo "$2"; then
          echo "curl policy: URL userinfo credentials are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_url_has_sensitive_query "$2"; then
          echo "curl policy: sensitive URL query parameters are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_url_has_fragment "$2"; then
          echo "curl policy: URL fragments are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        url="${url,,}"
        url_argument_count=$((url_argument_count + 1))
        if [[ "$url" == http://* || "$url" == https://* ]]; then
          url_count=$((url_count + 1))
        fi
        forwarded+=(--url "$2")
        shift 2
        ;;
      --url=*)
        url_argument="${1#*=}"
        if _tng_curl_url_has_userinfo "$url_argument"; then
          echo "curl policy: URL userinfo credentials are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_url_has_sensitive_query "$url_argument"; then
          echo "curl policy: sensitive URL query parameters are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_url_has_fragment "$url_argument"; then
          echo "curl policy: URL fragments are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        [[ "$url_argument" != @* ]] || {
          echo "curl policy: URL lists from files are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        }
        url="${url_argument,,}"
        url_argument_count=$((url_argument_count + 1))
        if [[ "$url" == http://* || "$url" == https://* ]]; then
          url_count=$((url_count + 1))
        fi
        forwarded+=(--url "$url_argument")
        shift
        ;;
      --url-query|--url-query=*)
        echo "curl policy: inline URL-query arguments are disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --next|-:)
        echo "curl policy: multiple transfer groups are disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --proto|--proto-redir|--proto=*|--proto-redir=*)
        echo "curl policy: caller-supplied protocol restrictions are disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --insecure|-k|--insecure=*)
        echo "curl policy: TLS certificate verification cannot be disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --no-globoff|--no-globoff=*)
        echo "curl policy: URL globbing cannot be enabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --proxy|-x|--proxy=*|-x?*|--proxy-*|-U|-U?*|--preproxy|--preproxy=*|--socks4|--socks4=*|--socks4a|--socks4a=*|--socks5|--socks5=*|--socks5-hostname|--socks5-hostname=*)
        echo "curl policy: explicit proxy routing is disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --connect-to|--connect-to=*|--resolve|--resolve=*|--unix-socket|--unix-socket=*|--abstract-unix-socket|--abstract-unix-socket=*|--doh-*|--dns-*|--alt-svc|--alt-svc=*)
        echo "curl policy: explicit route overrides are disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      --noproxy)
        if (($# < 2)); then
          echo "curl policy: missing --noproxy argument" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        shift 2
        ;;
      --noproxy=*)
        shift
        ;;
      -w|--write-out)
        if (($# < 2)); then
          echo "curl policy: missing argument for $1" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if ! _tng_curl_write_out_is_safe "$2"; then
          has_unsafe_writeout=1
        fi
        forwarded+=("$1" "$2")
        shift 2
        ;;
      -o|--output|-X|--request|-m|--max-time|--connect-timeout|--retry|--retry-delay|--retry-max-time|--speed-limit|--speed-time|--limit-rate|--max-filesize|--range|-r|--interface|--local-port|-A|--user-agent)
        if (($# < 2)); then
          echo "curl policy: missing argument for $1" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        forwarded+=("$1" "$2")
        shift 2
        ;;
      -w?*)
        write_out_format="${1:2}"
        if ! _tng_curl_write_out_is_safe "$write_out_format"; then
          has_unsafe_writeout=1
        fi
        forwarded+=("$1")
        shift
        ;;
      -o?*|-X?*|-m?*|-A?*|-r?*)
        forwarded+=("$1")
        shift
        ;;
      --write-out=*)
        write_out_format="${1#*=}"
        if ! _tng_curl_write_out_is_safe "$write_out_format"; then
          has_unsafe_writeout=1
        fi
        forwarded+=("$1")
        shift
        ;;
      --output=*|--request=*|--max-time=*|--connect-timeout=*|--retry=*|--retry-delay=*|--retry-max-time=*|--speed-limit=*|--speed-time=*|--limit-rate=*|--max-filesize=*|--range=*|--interface=*|--local-port=*|--user-agent=*)
        forwarded+=("$1")
        shift
        ;;
      --location-trusted)
        echo "curl policy: credential-forwarding redirects are disabled" >&2
        _tng_curl_cleanup_files "${private_files[@]}"
        return 2
        ;;
      -L|--location|--follow)
        has_redirect=1
        forwarded+=("$1")
        shift
        ;;
      *)
        url="${1,,}"
        if [[ "$1" != -* ]]; then
          url_argument_count=$((url_argument_count + 1))
        fi
        if [[ "$url" == http://* || "$url" == https://* ]]; then
          url_count=$((url_count + 1))
        fi
        if _tng_curl_url_has_userinfo "$1"; then
          echo "curl policy: URL userinfo credentials are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_url_has_sensitive_query "$1"; then
          echo "curl policy: sensitive URL query parameters are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if _tng_curl_url_has_fragment "$1"; then
          echo "curl policy: URL fragments are disabled" >&2
          _tng_curl_cleanup_files "${private_files[@]}"
          return 2
        fi
        if [[ "$1" =~ ^-[[:alpha:]]+$ ]]; then
          if [[ "$1" == *G* ]]; then
            has_get=1
          fi
          if [[ "$1" == *L* ]]; then
            has_redirect=1
          fi
          if [[ "$1" == *v* || "$1" == *i* ]]; then
            has_diagnostic_output=1
          fi
          if [[ "$1" == *k* ]]; then
            echo "curl policy: TLS certificate verification cannot be disabled" >&2
            _tng_curl_cleanup_files "${private_files[@]}"
            return 2
          fi
        fi
        case "$1" in
          --verbose|-v|--trace*|--include|--show-headers|-i|--dump-header*|-D|-D?*)
            has_diagnostic_output=1
            ;;
        esac
        forwarded+=("$1")
        shift
        ;;
    esac
  done

  if ((has_get && has_sensitive_query_data)); then
    echo "curl policy: credential-like query fields are disabled for GET data" >&2
    _tng_curl_cleanup_files "${private_files[@]}"
    return 2
  fi

  if ((has_get && has_uninspectable_query_data)); then
    echo "curl policy: file-backed GET data cannot be inspected safely" >&2
    _tng_curl_cleanup_files "${private_files[@]}"
    return 2
  fi

  if ((has_auth && has_redirect)); then
    echo "curl policy: redirects are disabled for requests with authentication headers" >&2
    _tng_curl_cleanup_files "${private_files[@]}"
    return 2
  fi

  if ((has_auth && (url_count != 1 || url_argument_count != 1))); then
    echo "curl policy: authenticated requests require exactly one explicit HTTP(S) URL" >&2
    _tng_curl_cleanup_files "${private_files[@]}"
    return 2
  fi

  if ((has_auth && has_diagnostic_output)); then
    echo "curl policy: diagnostic/header output is disabled for authenticated requests" >&2
    _tng_curl_cleanup_files "${private_files[@]}"
    return 2
  fi

  if ((has_auth && has_unsafe_writeout)); then
    echo "curl policy: authenticated write-out is limited to HTTP status and elapsed time" >&2
    _tng_curl_cleanup_files "${private_files[@]}"
    return 2
  fi

  if ((${#private_headers[@]})); then
    header_file="$(mktemp "${TMPDIR:-/tmp}/tng-curl-headers.XXXXXX")" || {
      _tng_curl_cleanup_files "${private_files[@]}"
      return 1
    }
    if ! chmod 600 -- "$header_file" || ! printf '%s\n' "${private_headers[@]}" > "$header_file"; then
      _tng_curl_cleanup_files "${private_files[@]}" "$header_file"
      return 1
    fi
    private_files+=("$header_file")
  fi

  if [[ -n "$header_file" ]]; then
    command curl -q --globoff --noproxy "*" --proto '=http,https' --proto-redir '=http,https' \
      -H "@$header_file" "${forwarded[@]}" || status=$?
  else
    command curl -q --globoff --noproxy "*" --proto '=http,https' --proto-redir '=http,https' \
      "${forwarded[@]}" || status=$?
  fi
  _tng_curl_cleanup_files "${private_files[@]}"
  return "$status"
}
