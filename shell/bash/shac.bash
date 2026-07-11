[[ -n "${SHAC_DISABLE:-}" ]] && return 0

if [[ -z "${_SHAC_BASH_LOADED:-}" ]]; then
  _SHAC_BASH_LOADED=1

  _shac_decode() {
    # Reverse wire-format v3 field encoding: \\->\, \t->tab, \n->LF, \r->CR.
    # An unrecognized escape (or a trailing lone backslash) passes through
    # literally, mirroring src/wire.rs::decode_field exactly.
    local s="$1"
    local -i i=0
    local -i len=${#s}
    local out="" c next
    while (( i < len )); do
      c="${s:i:1}"
      if [[ "$c" == "\\" ]]; then
        if (( i + 1 < len )); then
          next="${s:i+1:1}"
          case "$next" in
            "\\") out+="\\" ;;
            t) out+=$'\t' ;;
            n) out+=$'\n' ;;
            r) out+=$'\r' ;;
            *) out+="\\${next}" ;;
          esac
          i+=2
        else
          out+="\\"
          i+=1
        fi
      else
        out+="$c"
        i+=1
      fi
    done
    REPLY="$out"
  }

  _shac_bash_char_point() {
    # READLINE_POINT / COMP_POINT are BYTE offsets into the line (readline's
    # internal rl_point); src/context.rs treats --cursor as a CHARACTER
    # offset, so multibyte text before the cursor would otherwise desync the
    # active-token boundary the daemon computes. Slice the first N bytes
    # under the C locale (one byte == one "character" there, so the slice is
    # byte-exact), then measure that slice's length back under the normal
    # locale to get the character count.
    local full_line="$1"
    local -i byte_point="${2:-0}"
    (( byte_point < 0 )) && byte_point=0
    if (( byte_point == 0 )); then
      REPLY=0
      return
    fi
    local byte_prefix
    byte_prefix="$(printf '%s' "$full_line" | cut -b "1-${byte_point}" 2>/dev/null)"
    REPLY=${#byte_prefix}
  }

  # Splits `builtin history 1` output ("  N<ws>command...") into a number and
  # a command using bash parameter expansion only. sed's GNU-only `[0-9]\+`
  # BRE extension (the previous approach) is rejected by BSD sed -- macOS's
  # default /usr/bin/sed -- which silently fails to substitute, leaving
  # prompt-command recording permanently disabled there.
  _shac_history_split() {
    local raw="$1"
    local trimmed="${raw#"${raw%%[![:space:]]*}"}"
    local number="${trimmed%%[[:space:]]*}"
    [[ "$number" =~ ^[0-9]+$ ]] || number=""
    local rest="${trimmed#"$number"}"
    _shac_history_number="$number"
    _shac_history_command="${rest#"${rest%%[![:space:]]*}"}"
  }

  _shac_history_last_command() {
    _shac_history_split "$(builtin history 1 2>/dev/null)"
    printf '%s' "$_shac_history_command"
  }

  _shac_bash_active_region() {
    # Word spanning the cursor, honoring shell quoting: whitespace inside a
    # quoted (`"`/`'`) or backslash-escaped region does NOT split the token, so
    # `cat "my fi<Tab>` completes the whole `"my fi` span rather than just the
    # trailing `fi` and mangling the quote on splice (F6). Everything left of
    # the token goes in _shac_bash_before, everything from the token's end
    # onward in _shac_bash_after, so an accept replaces the whole active token.
    # Mirrors the zsh widget's _shac_active_token_span.
    local base_line="$1" base_point="$2"
    local -i len=${#base_line} i=0 start=0 end=${#base_line} escaped=0
    local quote="" c
    while (( i < len )); do
      c="${base_line:i:1}"
      if (( escaped )); then
        escaped=0
      elif [[ "$c" == "\\" ]]; then
        escaped=1
      elif [[ "$c" == '"' || "$c" == "'" ]]; then
        if [[ "$quote" == "$c" ]]; then
          quote=""
        elif [[ -z "$quote" ]]; then
          quote="$c"
        fi
      elif [[ -z "$quote" && "$c" == [[:space:]] ]]; then
        if (( i < base_point )); then
          # Whitespace left of the cursor: the token starts after it.
          start=$(( i + 1 ))
        elif (( end == len )); then
          # First whitespace at/after the cursor closes the token.
          end=$i
        fi
      fi
      i+=1
    done
    _shac_bash_before="${base_line:0:start}"
    _shac_bash_after="${base_line:end}"
  }

  _shac_bash_byte_length() {
    # READLINE_POINT is a BYTE offset (like COMP_POINT -- see
    # _shac_bash_char_point's comment), whereas `${#s}` counts CHARACTERS.
    # `wc -c` counts bytes regardless of locale (unlike `wc -m`/`${#s}`), so
    # route the length measurement through it instead of bash's own
    # character-counting expansion.
    REPLY=$(( $(printf '%s' "$1" | wc -c) ))
  }

  _shac_bash_splice_candidate() {
    local insert="${_shac_bash_candidates[$_shac_bash_cycle_index]}"
    READLINE_LINE="${_shac_bash_before}${insert}${_shac_bash_after}"
    _shac_bash_byte_length "${_shac_bash_before}${insert}"
    READLINE_POINT=$REPLY
    _shac_bash_expected_line="$READLINE_LINE"
    _shac_bash_expected_point="$READLINE_POINT"
  }

  _shac_bash_request() {
    # Issues the completion RPC for (line, cursor-in-characters) and fills
    # _shac_bash_candidates with decoded insert_text values (already
    # daemon-escaped for --shell bash). Shared by the bind -x (bash>=4) and
    # programmable-completion (bash<4) entry points below.
    local line="$1" cursor_chars="$2"
    local prev_command tty_value request_id resp_line
    prev_command="$(_shac_history_last_command)"
    tty_value="$(tty 2>/dev/null || true)"
    request_id=""
    _shac_bash_candidates=()

    while IFS= read -r resp_line; do
      [[ -z "$resp_line" ]] && continue
      if [[ "$resp_line" == __shac_request_id$'\t'* ]]; then
        local -a header
        IFS=$'\t' read -r -a header <<< "$resp_line"
        _shac_decode "${header[1]:-}"
        request_id="$REPLY"
      elif [[ "$resp_line" == __shac_*$'\t'* ]]; then
        # Skip other sentinel rows (e.g. __shac_tip) -- bash has no menu to
        # show a tip in, and they must not be mistaken for a candidate row.
        continue
      else
        # A candidate row always has tab-separated fields; a tab-less line (a
        # stray daemon status message on stdout) is not a candidate (F1).
        [[ "$resp_line" != *$'\t'* ]] && continue
        local -a fields
        IFS=$'\t' read -r -a fields <<< "$resp_line"
        _shac_decode "${fields[1]:-}"
        _shac_bash_candidates+=("$REPLY")
      fi
    done < <(TTY="$tty_value" shac complete --shell bash --line "$line" --cursor "$cursor_chars" --cwd "$PWD" --prev-command "$prev_command" --format shell-tsv-v3 2>/dev/null)

    if [[ -n "$request_id" ]]; then
      _shac_last_request_id="$request_id"
      _shac_last_completion_line="$line"
      _shac_last_completion_ts="$(date +%s)"
    fi
  }

  _shac_bash_complete() {
    local line="$READLINE_LINE" point="$READLINE_POINT"

    # Repeat press with nothing typed/moved since our last splice: cycle to
    # the next candidate from the same request instead of asking again.
    if [[ ${#_shac_bash_candidates[@]} -gt 0 \
          && "$line" == "${_shac_bash_expected_line:-}" \
          && "$point" == "${_shac_bash_expected_point:-}" ]]; then
      _shac_bash_cycle_index=$(( (_shac_bash_cycle_index + 1) % ${#_shac_bash_candidates[@]} ))
      _shac_bash_splice_candidate
      return
    fi

    _shac_bash_active_region "$line" "$point"
    _shac_bash_cycle_index=0

    # READLINE_POINT is a byte offset; --cursor wants characters (F9).
    local cursor_chars
    _shac_bash_char_point "$line" "$point"
    cursor_chars="$REPLY"
    _shac_bash_request "$line" "$cursor_chars"

    [[ ${#_shac_bash_candidates[@]} -eq 0 ]] && return
    _shac_bash_splice_candidate
  }

  _shac_bash_unescape() {
    # Reverse src/quote.rs::quote_token's Shell::Bash output back to the raw
    # token. Only the bash<4 COMPREPLY fallback needs this (F6): compopt is
    # unavailable on bash<4, so "-o filenames" is set once at registration
    # time instead and readline re-quotes whatever we hand it -- if that's
    # the already-escaped insert_text, readline would quote it a second
    # time. Handles the three shapes quote_token can emit for bash: a
    # self-contained double-quoted literal, a self-contained single-quoted
    # literal, and the common bare-tilde/backslash-escaped-tail form.
    local tok="$1"
    local -i len=${#tok}
    # Negative string-offset syntax (${tok: -1}) needs bash>=4.2, but this
    # helper's only caller is the bash<4 fallback -- index from len instead.

    if (( len >= 2 )) && [[ "${tok:0:1}" == '"' && "${tok:len-1:1}" == '"' ]]; then
      _shac_bash_unescape_dquote "${tok:1:len-2}"
      return
    fi
    if (( len >= 2 )) && [[ "${tok:0:1}" == "'" && "${tok:len-1:1}" == "'" ]]; then
      _shac_bash_unescape_squote "${tok:1:len-2}"
      return
    fi
    _shac_bash_unescape_bare "$tok"
  }

  _shac_bash_unescape_dquote() {
    # Content of a self-contained "..." literal: \" \\ \$ \` are the only
    # escapes this shape ever emits (mirrors quote.rs's open_quote='"' arm).
    local s="$1" out="" c next
    local -i i=0 n=${#s}
    while (( i < n )); do
      c="${s:i:1}"
      if [[ "$c" == "\\" ]] && (( i + 1 < n )); then
        next="${s:i+1:1}"
        case "$next" in
          '"'|'\'|'$'|'`') out+="$next"; i+=2 ;;
          *) out+="$c"; i+=1 ;;
        esac
      else
        out+="$c"; i+=1
      fi
    done
    REPLY="$out"
  }

  _shac_bash_unescape_squote() {
    # Content of a self-contained '...' literal: every embedded ' was
    # spliced as close/backslash-escaped-quote/reopen ('\''); nothing else
    # is ever escaped inside single quotes.
    local s="$1" out=""
    local -i i=0 n=${#s}
    while (( i < n )); do
      if [[ "${s:i:4}" == "'\\''" ]]; then
        out+="'"
        i+=4
      else
        out+="${s:i:1}"
        i+=1
      fi
    done
    REPLY="$out"
  }

  _shac_bash_unescape_bare() {
    # No open quote: quote_token may keep a leading ~/ or $HOME/ prefix
    # bare (for shell expansion) and backslash/ANSI-C-escapes the rest.
    # Expand the bare prefix to an absolute $HOME path here -- the
    # COMPREPLY fallback needs an absolute, requotable value (spec section
    # 7 contingency) since there is no compopt to special-case a literal ~.
    local tok="$1"

    if [[ "$tok" == '$HOME/'* ]]; then
      _shac_bash_unescape_tail "${tok:6}"
      REPLY="${HOME}/${REPLY}"
      return
    fi
    if [[ "$tok" == '$HOME' ]]; then
      REPLY="${HOME}"
      return
    fi
    if [[ "$tok" == "~/"* ]]; then
      _shac_bash_unescape_tail "${tok:2}"
      REPLY="${HOME}/${REPLY}"
      return
    fi
    if [[ "$tok" == "~" ]]; then
      REPLY="${HOME}"
      return
    fi

    _shac_bash_unescape_tail "$tok"
  }

  _shac_bash_unescape_tail() {
    # Reverse escape_word's Shell::Bash output: a literal backslash always
    # precedes an escaped character; a bare (unescaped) '$' immediately
    # followed by "'" only ever appears here as the start of one of its
    # ANSI-C control-byte spans ($'\t' $'\n' $'\r' $'\xHH'), since a literal
    # '$' is always escaped to \$.
    local s="$1" out="" c
    local -i i=0 n=${#s}
    while (( i < n )); do
      c="${s:i:1}"
      if [[ "$c" == '$' && "${s:i+1:1}" == "'" && "${s:i+2:1}" == "\\" ]]; then
        local code="${s:i+3:1}"
        case "$code" in
          t) if [[ "${s:i+4:1}" == "'" ]]; then out+=$'\t'; i+=5; else out+="$c"; i+=1; fi ;;
          n) if [[ "${s:i+4:1}" == "'" ]]; then out+=$'\n'; i+=5; else out+="$c"; i+=1; fi ;;
          r) if [[ "${s:i+4:1}" == "'" ]]; then out+=$'\r'; i+=5; else out+="$c"; i+=1; fi ;;
          x)
            local hex="${s:i+4:2}" byte
            if [[ "$hex" =~ ^[0-9a-fA-F][0-9a-fA-F]$ && "${s:i+6:1}" == "'" ]]; then
              printf -v byte "\\x$hex"
              out+="$byte"
              i+=7
            else
              out+="$c"; i+=1
            fi
            ;;
          *) out+="$c"; i+=1 ;;
        esac
      elif [[ "$c" == "\\" ]] && (( i + 1 < n )); then
        out+="${s:i+1:1}"
        i+=2
      else
        out+="$c"
        i+=1
      fi
    done
    REPLY="$out"
  }

  _shac_bash_complete_legacy() {
    # bash < 4.0 (e.g. macOS's default bash 3.2) never exposes
    # READLINE_LINE/READLINE_POINT to bind -x, so _shac_bash_complete above
    # is unreachable there (F6). Fall back to bash's older
    # programmable-completion protocol: COMP_LINE/COMP_POINT in, COMPREPLY
    # out -- readline itself quotes COMPREPLY entries for us because
    # "-o filenames" is set on the compspec at registration time below.
    COMPREPLY=()

    # COMP_POINT is a byte offset; --cursor wants characters (F9). This is
    # the only path testable on this host (macOS ships bash 3.2).
    local cursor_chars
    _shac_bash_char_point "$COMP_LINE" "$COMP_POINT"
    cursor_chars="$REPLY"
    _shac_bash_request "$COMP_LINE" "$cursor_chars"

    [[ ${#_shac_bash_candidates[@]} -eq 0 ]] && return

    local raw
    for raw in "${_shac_bash_candidates[@]}"; do
      _shac_bash_unescape "$raw"
      COMPREPLY+=("$REPLY")
    done
  }

  _shac_record_prompt_command() {
    local history_line now provenance
    history_line="$(builtin history 1 2>/dev/null)"
    _shac_history_split "$history_line"
    local history_number="$_shac_history_number"
    local command="$_shac_history_command"

    if [[ -z "$history_number" || "$history_number" == "$_shac_last_history_number" || -z "$command" ]]; then
      return
    fi
    _shac_last_history_number="$history_number"

    if [[ "$command" == shac\ * || "$command" == _shac_* ]]; then
      return
    fi

    provenance="typed_manual"
    now="$(date +%s)"
    if [[ -n "${_shac_last_request_id:-}" && -n "${_shac_last_completion_ts:-}" ]]; then
      if (( now - _shac_last_completion_ts <= 30 )) && [[ -n "${_shac_last_completion_line:-}" ]] && [[ "$command" == "${_shac_last_completion_line}"* ]]; then
        provenance="accepted_completion"
      fi
    fi

    local -a cmd
    cmd=(
      shac record-command
      --shell bash
      --cwd "$PWD"
      --command "$command"
      --trust interactive
      --provenance "$provenance"
      --origin bash_prompt_command
      --tty-present
    )
    if [[ "$provenance" == "accepted_completion" && -n "${_shac_last_request_id:-}" ]]; then
      cmd+=(--accepted-request-id "$_shac_last_request_id")
    fi
    "${cmd[@]}" >/dev/null 2>&1

    _shac_last_request_id=""
    _shac_last_completion_line=""
    _shac_last_completion_ts=""
  }

  if declare -p PROMPT_COMMAND >/dev/null 2>&1 && [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    case " ${PROMPT_COMMAND[*]} " in
      *" _shac_record_prompt_command "*) ;;
      *) PROMPT_COMMAND=(_shac_record_prompt_command "${PROMPT_COMMAND[@]}") ;;
    esac
  elif [[ -n "${PROMPT_COMMAND:-}" ]]; then
    case ";${PROMPT_COMMAND};" in
      *";_shac_record_prompt_command;"*) ;;
      *) PROMPT_COMMAND="_shac_record_prompt_command; ${PROMPT_COMMAND}" ;;
    esac
  else
    PROMPT_COMMAND="_shac_record_prompt_command"
  fi

  if (( ${BASH_VERSINFO[0]:-0} >= 4 )); then
    # bash >= 4.0: READLINE_LINE/READLINE_POINT exist, so bind -x can edit
    # the command line directly (see _shac_bash_complete).
    bind -x '"\t": _shac_bash_complete'
  else
    # bash < 4.0: no READLINE_LINE/READLINE_POINT for bind -x (F6). Fall
    # back to programmable completion; -o filenames lets readline quote the
    # raw (unescaped) values _shac_bash_complete_legacy hands it.
    if ! complete -o filenames -o bashdefault -o default -F _shac_bash_complete_legacy -D 2>/dev/null; then
      # -D (default completion, any command) is itself a bash>=4.0 addition
      # -- macOS's frozen bash 3.2 predates it too, so `-D` errors here.
      # Register the fallback against a fixed list of common commands
      # instead of giving up on completion entirely.
      complete -o filenames -o bashdefault -o default -F _shac_bash_complete_legacy \
        cd ls cat less more head tail cp mv rm mkdir touch chmod find grep rg \
        tar git ssh scp rsync python python3 node npm pip pip3 docker kubectl \
        make cargo go vim nvim nano code open du df diff
    fi
  fi
fi
