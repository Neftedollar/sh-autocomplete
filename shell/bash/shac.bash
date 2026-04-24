[[ -n "${SHAC_DISABLE:-}" ]] && return 0

if [[ -z "${_SHAC_BASH_LOADED:-}" ]]; then
  _SHAC_BASH_LOADED=1

  _shac_complete() {
    local line point prev_command item request_id tty_value
    COMPREPLY=()
    line="${COMP_LINE}"
    point="${COMP_POINT}"
    prev_command="$(_shac_history_last_command)"
    request_id=""
    tty_value="$(tty 2>/dev/null || true)"

    while IFS= read -r item; do
      if [[ "$item" == $'__shac_request_id\t'* ]]; then
        request_id="${item#*$'\t'}"
      elif [[ -n "$item" ]]; then
        COMPREPLY+=("$item")
      fi
    done < <(TTY="$tty_value" shac complete --shell bash --line "$line" --cursor "$point" --cwd "$PWD" --prev-command "$prev_command" --format shell-metadata 2>/dev/null)

    if [[ -n "$request_id" ]]; then
      _shac_last_request_id="$request_id"
      _shac_last_completion_line="$line"
      _shac_last_completion_ts="$(date +%s)"
    fi
  }

  _shac_history_last_command() {
    builtin history 1 2>/dev/null | sed 's/^[[:space:]]*[0-9]\+[[:space:]]*//'
  }

  _shac_record_prompt_command() {
    local history_line history_number command now provenance
    history_line="$(builtin history 1 2>/dev/null)"
    history_number="$(printf '%s\n' "$history_line" | sed -n 's/^[[:space:]]*\([0-9]\+\).*/\1/p')"
    command="$(printf '%s\n' "$history_line" | sed 's/^[[:space:]]*[0-9]\+[[:space:]]*//')"

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

  complete -o nosort -o bashdefault -o default -F _shac_complete -D
fi
