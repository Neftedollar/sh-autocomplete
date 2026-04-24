[[ -n "${SHAC_DISABLE:-}" ]] && return 0

if [[ -z "${_SHAC_ZSH_LOADED:-}" ]]; then
  _SHAC_ZSH_LOADED=1

  typeset -g _shac_last_request_id=""
  typeset -g _shac_last_recorded=""
  typeset -g _shac_last_buffer=""
  typeset -g _shac_input_provenance="unknown"
  typeset -g _shac_input_provenance_source="unknown"
  typeset -g _shac_input_provenance_confidence="unknown"
  typeset -g _shac_preexec_provenance="unknown"
  typeset -g _shac_preexec_provenance_source="unknown"
  typeset -g _shac_preexec_provenance_confidence="unknown"
  typeset -g _shac_preexec_request_id=""
  typeset -g _shac_preexec_item_key=""
  typeset -g _shac_preexec_rank=""
  typeset -g _shac_last_accepted_item_key=""
  typeset -g _shac_last_accepted_rank=""
  typeset -g _shac_completion_edited=0
  typeset -gi _shac_edit_event_count=0
  typeset -gi _shac_menu_open=0
  typeset -gi _shac_menu_selected_index=0
  typeset -gi _shac_menu_original_cursor=0
  typeset -g _shac_menu_original_buffer=""
  typeset -ga _shac_menu_item_keys=()
  typeset -ga _shac_menu_insert_texts=()
  typeset -ga _shac_menu_displays=()
  typeset -ga _shac_menu_kinds=()
  typeset -ga _shac_menu_sources=()
  typeset -ga _shac_menu_descriptions=()

  function _shac_reset_accept_state() {
    _shac_last_request_id=""
    _shac_last_accepted_item_key=""
    _shac_last_accepted_rank=""
    _shac_completion_edited=0
    _shac_edit_event_count=0
  }

  function _shac_reset_menu_state() {
    _shac_menu_open=0
    _shac_menu_selected_index=0
    _shac_menu_original_cursor=0
    _shac_menu_original_buffer=""
    _shac_menu_item_keys=()
    _shac_menu_insert_texts=()
    _shac_menu_displays=()
    _shac_menu_kinds=()
    _shac_menu_sources=()
    _shac_menu_descriptions=()
  }

  function _shac_set_input_provenance() {
    _shac_input_provenance="$1"
    _shac_input_provenance_source="${2:-unknown}"
    _shac_input_provenance_confidence="${3:-unknown}"
  }

  function _shac_mark_pasted() {
    _shac_set_input_provenance "pasted" "${1:-unknown}" "${2:-unknown}"
    if [[ -n "$_shac_last_request_id" ]]; then
      _shac_completion_edited=1
      _shac_last_accepted_item_key=""
      _shac_last_accepted_rank=""
    fi
  }

  function _shac_mark_typed_manual() {
    _shac_set_input_provenance "typed_manual" "unknown" "unknown"
    _shac_edit_event_count=$(( _shac_edit_event_count + 1 ))
    if [[ -n "$_shac_last_request_id" ]]; then
      _shac_completion_edited=1
      _shac_last_accepted_item_key=""
      _shac_last_accepted_rank=""
    fi
  }

  function _shac_maybe_mark_heuristic_paste_from_diff() {
    local before_buffer="$1"
    local after_buffer="$2"
    local delta=$(( ${#after_buffer} - ${#before_buffer} ))

    if [[ "$_shac_input_provenance" == "accepted_completion" ]]; then
      return 1
    fi
    if [[ "$after_buffer" == *$'\n'* ]] || (( delta > 1 )); then
      _shac_mark_pasted "zsh_paste_heuristic" "heuristic"
      return 0
    fi
    return 1
  }

  function _shac_maybe_mark_heuristic_paste_from_buffer() {
    if [[ "$_shac_input_provenance" != "unknown" ]]; then
      return 1
    fi
    if (( _shac_menu_open )); then
      return 1
    fi
    if [[ "$BUFFER" == *$'\n'* ]]; then
      _shac_mark_pasted "zsh_paste_heuristic" "heuristic"
      return 0
    fi
    if (( _shac_edit_event_count == 0 )) && (( ${#BUFFER} >= 16 )) && [[ "$BUFFER" == *[[:space:]]* ]]; then
      _shac_mark_pasted "zsh_paste_heuristic" "heuristic"
      return 0
    fi
    return 1
  }

  function _shac_clear_menu_display() {
    POSTDISPLAY=""
    if zle; then
      zle -R -c
    fi
  }

  function _shac_close_menu() {
    local restore_original="${1:-0}"
    if (( _shac_menu_open )) && (( restore_original )); then
      BUFFER="$_shac_menu_original_buffer"
      CURSOR=$_shac_menu_original_cursor
    fi
    _shac_clear_menu_display
    _shac_reset_menu_state
  }

  function _shac_preview_buffer_for_item() {
    local base_buffer="$1"
    local base_cursor="$2"
    local insert_text="$3"
    local left right token_prefix before_token token_suffix after_token

    if (( base_cursor <= 0 )); then
      left=""
    else
      left="${base_buffer[1,base_cursor]}"
    fi

    if (( base_cursor >= ${#base_buffer} )); then
      right=""
    else
      right="${base_buffer[$(( base_cursor + 1 )),-1]}"
    fi

    token_prefix="${left##*[[:space:]]}"
    if (( ${#token_prefix} > 0 )); then
      before_token="${left[1,$(( ${#left} - ${#token_prefix} ))]}"
    else
      before_token="$left"
    fi

    token_suffix="${right%%[[:space:]]*}"
    after_token="${right#$token_suffix}"

    REPLY="${before_token}${insert_text}${after_token}"
    REPLY2=$(( ${#before_token} + ${#insert_text} ))
  }

  function _shac_apply_selected_item() {
    local index="$1"
    local base_buffer="$2"
    local base_cursor="$3"
    local insert_text="${_shac_menu_insert_texts[$index]}"

    _shac_preview_buffer_for_item "$base_buffer" "$base_cursor" "$insert_text"
    BUFFER="$REPLY"
    CURSOR="$REPLY2"
    _shac_last_request_id="${_shac_last_request_id:-}"
    _shac_last_accepted_item_key="${_shac_menu_item_keys[$index]}"
    _shac_last_accepted_rank="$(( index - 1 ))"
    _shac_set_input_provenance "accepted_completion" "unknown" "unknown"
    _shac_completion_edited=0
  }

  function _shac_selected_kind() {
    REPLY="${_shac_menu_kinds[$_shac_menu_selected_index]:-}"
  }

  function _shac_selected_source() {
    REPLY="${_shac_menu_sources[$_shac_menu_selected_index]:-}"
  }

  function _shac_selected_item_key() {
    REPLY="${_shac_menu_item_keys[$_shac_menu_selected_index]:-}"
  }

  function _shac_selected_requires_more_input() {
    local kind="$1"
    local insert_text="$2"

    case "$kind" in
      option)
        [[ "$insert_text" == "-m" || "$insert_text" == "-c" || "$insert_text" == --*= ]] && return 0
        return 1
        ;;
      subcommand|module)
        return 0
        ;;
      *)
        return 1
        ;;
    esac
  }

  function _shac_selected_is_full_line() {
    local source="$1"
    local item_key="$2"
    [[ "$source" == "history" || "$source" == "runtime_history" || "$source" == "transition" ]] && [[ "$item_key" == *" "* ]]
  }

  function _shac_buffer_ends_with_space() {
    [[ "$BUFFER" == *[[:space:]] ]]
  }

  function _shac_commit_selected_item() {
    local add_space="${1:-auto}"
    if (( !_shac_menu_open )); then
      return 1
    fi

    local index="$_shac_menu_selected_index"
    local insert_text="${_shac_menu_insert_texts[$index]}"
    local kind="${_shac_menu_kinds[$index]}"

    _shac_apply_selected_item "$index" "$_shac_menu_original_buffer" "$_shac_menu_original_cursor"
    if [[ "$add_space" == "always" ]] || { [[ "$add_space" == "auto" ]] && _shac_selected_requires_more_input "$kind" "$insert_text"; }; then
      if ! _shac_buffer_ends_with_space; then
        BUFFER="${BUFFER} "
        CURSOR=$(( CURSOR + 1 ))
      fi
    fi
    _shac_close_menu 0
    if zle; then
      zle -R
    fi
    return 0
  }

  function _shac_render_menu() {
    local total="${#_shac_menu_item_keys[@]}"
    if (( total == 0 )); then
      _shac_clear_menu_display
      return
    fi

    local limit=8
    local start=1
    local end=$total
    if (( total > limit )); then
      start=$(( _shac_menu_selected_index - (limit / 2) ))
      if (( start < 1 )); then
        start=1
      fi
      end=$(( start + limit - 1 ))
      if (( end > total )); then
        end=$total
        start=$(( end - limit + 1 ))
      fi
    fi

    local -a lines
    lines+=("shac ${_shac_menu_selected_index}/${total}")

    local i marker display kind source description line
    for (( i = start; i <= end; i++ )); do
      marker=" "
      if (( i == _shac_menu_selected_index )); then
        marker=">"
      fi
      display="${_shac_menu_displays[$i]}"
      kind="${_shac_menu_kinds[$i]}"
      source="${_shac_menu_sources[$i]}"
      description="${_shac_menu_descriptions[$i]}"
      line="${marker} ${display}"
      if [[ -n "$kind" || -n "$source" ]]; then
        line="${line} [${kind}${source:+/$source}]"
      fi
      if [[ -n "$description" ]]; then
        line="${line} -- ${description}"
      fi
      lines+=("$line")
    done

    POSTDISPLAY=$'\n'"${(F)lines}"
    if zle; then
      zle -R
    fi
  }

  function _shac_menu_step() {
    local delta="$1"
    local total="${#_shac_menu_item_keys[@]}"
    if (( !_shac_menu_open || total == 0 )); then
      return 1
    fi

    local next=$(( _shac_menu_selected_index + delta ))
    if (( next < 1 )); then
      next=$total
    elif (( next > total )); then
      next=1
    fi
    _shac_menu_selected_index=$next
    _shac_apply_selected_item "$_shac_menu_selected_index" "$_shac_menu_original_buffer" "$_shac_menu_original_cursor"
    _shac_render_menu
    return 0
  }

  function _shac_fetch_candidates() {
    _shac_reset_menu_state
    local tty_value line history_line
    local -a runtime_history_args
    tty_value="$(tty 2>/dev/null || true)"

    while IFS= read -r history_line; do
      [[ -z "$history_line" ]] && continue
      [[ "$history_line" == shac\ * || "$history_line" == _shac_* ]] && continue
      runtime_history_args+=(--history-command "$history_line")
    done < <(fc -ln -20 2>/dev/null | sed 's/^[[:space:]]*//' | tail -n 20)

    while IFS= read -r line; do
      [[ -z "$line" ]] && continue
      if [[ "$line" == __shac_request_id$'\t'* ]]; then
        local -a header
        header=("${(ps:\t:)line}")
        _shac_last_request_id="${header[2]:-}"
      else
        local -a fields
        fields=("${(ps:\t:)line}")
        _shac_menu_item_keys+=("${fields[1]:-}")
        _shac_menu_insert_texts+=("${fields[2]:-}")
        _shac_menu_displays+=("${fields[3]:-}")
        _shac_menu_kinds+=("${fields[4]:-}")
        _shac_menu_sources+=("${fields[5]:-}")
        _shac_menu_descriptions+=("${fields[6]:-}")
      fi
    done < <(
      TTY="$tty_value" shac complete \
        --shell zsh \
        --line "$BUFFER" \
        --cursor "$CURSOR" \
        --cwd "$PWD" \
        --prev-command "$(fc -ln -1 2>/dev/null | sed 's/^[[:space:]]*//')" \
        "${runtime_history_args[@]}" \
        --format shell-tsv-v2 \
        2>/dev/null
    )

    [[ -n "$_shac_last_request_id" || ${#_shac_menu_item_keys[@]} -gt 0 ]]
  }

  function _shac_fallback_complete() {
    _shac_close_menu 0
    zle _shac_orig_expand_or_complete -- "$@"
  }

  function _shac_complete() {
    local -a results
    if ! _shac_fetch_candidates; then
      _default
      return $?
    fi
    if (( ${#_shac_menu_displays[@]} == 0 )); then
      _default
      return $?
    fi
    results=("${_shac_menu_displays[@]}")
    compadd -Q -- "${results[@]}"
  }

  function _shac_open_menu() {
    if (( ${#_shac_menu_item_keys[@]} == 0 )); then
      return 1
    fi

    _shac_menu_open=1
    _shac_menu_selected_index=1
    _shac_menu_original_buffer="$BUFFER"
    _shac_menu_original_cursor="$CURSOR"
    _shac_apply_selected_item 1 "$_shac_menu_original_buffer" "$_shac_menu_original_cursor"
    _shac_render_menu
    return 0
  }

  function _shac_tab_widget() {
    if (( _shac_menu_open )); then
      _shac_menu_step 1
      return $?
    fi

    if [[ "$BUFFER" == *$'\n'* ]]; then
      _shac_fallback_complete
      return $?
    fi

    if ! _shac_fetch_candidates; then
      _shac_fallback_complete
      return $?
    fi

    local total="${#_shac_menu_item_keys[@]}"
    if (( total == 0 )); then
      _shac_fallback_complete
      return $?
    fi

    if (( total == 1 )); then
      local original_buffer="$BUFFER"
      local original_cursor="$CURSOR"
      _shac_apply_selected_item 1 "$original_buffer" "$original_cursor"
      if zle; then
        zle -R
      fi
      return 0
    fi

    _shac_open_menu
  }

  function _shac_shift_tab_widget() {
    if (( _shac_menu_open )); then
      _shac_menu_step -1
      return $?
    fi

    if zle -l reverse-menu-complete >/dev/null 2>&1; then
      zle _shac_orig_reverse_menu_complete -- "$@"
    else
      zle _shac_orig_expand_or_complete -- "$@"
    fi
  }

  function _shac_up_widget() {
    if (( _shac_menu_open )); then
      _shac_menu_step -1
      return $?
    fi
    zle _shac_orig_up_line_or_history -- "$@"
  }

  function _shac_down_widget() {
    if (( _shac_menu_open )); then
      _shac_menu_step 1
      return $?
    fi
    zle _shac_orig_down_line_or_history -- "$@"
  }

  function _shac_cancel_menu_widget() {
    if (( _shac_menu_open )); then
      _shac_close_menu 1
      _shac_reset_accept_state
      _shac_set_input_provenance "unknown" "unknown" "unknown"
      if zle; then
        zle -R
      fi
      return 0
    fi
    zle _shac_orig_send_break -- "$@"
  }

  function _shac_note_manual_edit() {
    if (( _shac_menu_open )); then
      _shac_close_menu 1
    fi
    _shac_mark_typed_manual
  }

  function _shac_self_insert_widget() {
    local before_buffer="$BUFFER"
    if (( _shac_menu_open )); then
      _shac_close_menu 1
    fi
    zle _shac_orig_self_insert -- "$@"
    if ! _shac_maybe_mark_heuristic_paste_from_diff "$before_buffer" "$BUFFER"; then
      _shac_mark_typed_manual
    fi
  }

  function _shac_backward_delete_char_widget() {
    _shac_note_manual_edit
    zle _shac_orig_backward_delete_char -- "$@"
  }

  function _shac_delete_char_widget() {
    _shac_note_manual_edit
    zle _shac_orig_delete_char -- "$@"
  }

  function _shac_kill_word_widget() {
    _shac_note_manual_edit
    zle _shac_orig_kill_word -- "$@"
  }

  function _shac_backward_kill_word_widget() {
    _shac_note_manual_edit
    zle _shac_orig_backward_kill_word -- "$@"
  }

  function _shac_forward_char_widget() {
    if (( _shac_menu_open )); then
      _shac_commit_selected_item never
      return $?
    fi
    zle _shac_orig_forward_char -- "$@"
  }

  function _shac_backward_char_widget() {
    if (( _shac_menu_open )); then
      _shac_close_menu 1
    fi
    zle _shac_orig_backward_char -- "$@"
  }

  function _shac_bracketed_paste_widget() {
    if (( _shac_menu_open )); then
      _shac_close_menu 1
    fi
    _shac_mark_pasted "zsh_bracketed_paste" "exact"
    zle _shac_orig_bracketed_paste -- "$@"
  }

  function _shac_accept_line_widget() {
    if (( _shac_menu_open )); then
      local source item_key
      _shac_selected_source
      source="$REPLY"
      _shac_selected_item_key
      item_key="$REPLY"
      if _shac_selected_is_full_line "$source" "$item_key"; then
        _shac_preexec_provenance="accepted_completion"
        _shac_preexec_provenance_source="unknown"
        _shac_preexec_provenance_confidence="unknown"
        _shac_preexec_request_id="$_shac_last_request_id"
        _shac_preexec_item_key="$_shac_last_accepted_item_key"
        _shac_preexec_rank="$_shac_last_accepted_rank"
        _shac_close_menu 0
        zle _shac_orig_accept_line -- "$@"
        return $?
      fi
      _shac_commit_selected_item auto
      return $?
    fi

    _shac_maybe_mark_heuristic_paste_from_buffer

    if [[ "$_shac_input_provenance" == "accepted_completion" && "$_shac_completion_edited" -eq 0 && -n "$_shac_last_request_id" ]]; then
      _shac_preexec_provenance="accepted_completion"
      _shac_preexec_provenance_source="unknown"
      _shac_preexec_provenance_confidence="unknown"
      _shac_preexec_request_id="$_shac_last_request_id"
      _shac_preexec_item_key="$_shac_last_accepted_item_key"
      _shac_preexec_rank="$_shac_last_accepted_rank"
    elif [[ "$_shac_input_provenance" == "pasted" ]]; then
      _shac_preexec_provenance="pasted"
      _shac_preexec_provenance_source="${_shac_input_provenance_source:-unknown}"
      _shac_preexec_provenance_confidence="${_shac_input_provenance_confidence:-unknown}"
      _shac_preexec_request_id=""
      _shac_preexec_item_key=""
      _shac_preexec_rank=""
    elif [[ "$_shac_input_provenance" == "typed_manual" ]]; then
      _shac_preexec_provenance="typed_manual"
      _shac_preexec_provenance_source="unknown"
      _shac_preexec_provenance_confidence="unknown"
      _shac_preexec_request_id=""
      _shac_preexec_item_key=""
      _shac_preexec_rank=""
    else
      _shac_preexec_provenance="unknown"
      _shac_preexec_provenance_source="unknown"
      _shac_preexec_provenance_confidence="unknown"
      _shac_preexec_request_id=""
      _shac_preexec_item_key=""
      _shac_preexec_rank=""
    fi
    zle _shac_orig_accept_line -- "$@"
  }

  function _shac_space_widget() {
    if (( _shac_menu_open )); then
      _shac_commit_selected_item always
      return $?
    fi
    zle _shac_orig_self_insert -- "$@"
  }

  function _shac_record_precmd() {
    if [[ -z "$_shac_last_buffer" || "$_shac_last_buffer" == "$_shac_last_recorded" ]]; then
      return
    fi
    if [[ "$_shac_last_buffer" != shac\ * && "$_shac_last_buffer" != _shac_* ]]; then
      local -a cmd
      cmd=(
        shac record-command
        --shell zsh
        --cwd "$PWD"
        --command "$_shac_last_buffer"
        --trust interactive
        --provenance "${_shac_preexec_provenance:-unknown}"
        --provenance-source "${_shac_preexec_provenance_source:-unknown}"
        --provenance-confidence "${_shac_preexec_provenance_confidence:-unknown}"
        --origin zsh_precmd
        --tty-present
      )
      if [[ -n "$_shac_preexec_request_id" ]]; then
        cmd+=(--accepted-request-id "$_shac_preexec_request_id")
      fi
      if [[ -n "$_shac_preexec_item_key" ]]; then
        cmd+=(--accepted-item-key "$_shac_preexec_item_key")
      fi
      if [[ -n "$_shac_preexec_rank" ]]; then
        cmd+=(--accepted-rank "$_shac_preexec_rank")
      fi
      "${cmd[@]}" >/dev/null 2>&1
    fi
    _shac_last_recorded="$_shac_last_buffer"
    _shac_last_buffer=""
    _shac_preexec_provenance="unknown"
    _shac_preexec_provenance_source="unknown"
    _shac_preexec_provenance_confidence="unknown"
    _shac_preexec_request_id=""
    _shac_preexec_item_key=""
    _shac_preexec_rank=""
    _shac_reset_accept_state
    _shac_close_menu 0
    _shac_set_input_provenance "unknown" "unknown" "unknown"
  }

  function _shac_capture_preexec() {
    if [[ -n "$1" ]]; then
      _shac_last_buffer="$1"
    fi
  }

  if [[ "${SHAC_ZSH_TEST_MODE:-0}" != "1" ]]; then
    autoload -Uz compinit
    if ! typeset -p _comps >/dev/null 2>&1; then
      compinit
    fi
    autoload -Uz +X _default
    autoload -Uz add-zsh-hook

    compdef _shac_complete -command- -default-

    zle -A self-insert _shac_orig_self_insert
    zle -N self-insert _shac_self_insert_widget
    zle -N _shac_space_widget _shac_space_widget
    bindkey ' ' _shac_space_widget
    zle -A backward-delete-char _shac_orig_backward_delete_char
    zle -N backward-delete-char _shac_backward_delete_char_widget
    zle -A delete-char _shac_orig_delete_char
    zle -N delete-char _shac_delete_char_widget
    zle -A kill-word _shac_orig_kill_word
    zle -N kill-word _shac_kill_word_widget
    zle -A backward-kill-word _shac_orig_backward_kill_word
    zle -N backward-kill-word _shac_backward_kill_word_widget
    zle -A forward-char _shac_orig_forward_char
    zle -N forward-char _shac_forward_char_widget
    zle -A backward-char _shac_orig_backward_char
    zle -N backward-char _shac_backward_char_widget
    if zle -la bracketed-paste >/dev/null 2>&1; then
      zle -A bracketed-paste _shac_orig_bracketed_paste
      zle -N bracketed-paste _shac_bracketed_paste_widget
    fi
    zle -A accept-line _shac_orig_accept_line
    zle -N accept-line _shac_accept_line_widget
    zle -A expand-or-complete _shac_orig_expand_or_complete
    zle -N _shac_tab_widget _shac_tab_widget
    bindkey '^I' _shac_tab_widget
    bindkey '^F' _shac_forward_char_widget
    if zle -la reverse-menu-complete >/dev/null 2>&1; then
      zle -A reverse-menu-complete _shac_orig_reverse_menu_complete
    fi
    zle -N _shac_shift_tab_widget _shac_shift_tab_widget
    bindkey '^[[Z' _shac_shift_tab_widget
    zle -A up-line-or-history _shac_orig_up_line_or_history
    zle -N up-line-or-history _shac_up_widget
    zle -A down-line-or-history _shac_orig_down_line_or_history
    zle -N down-line-or-history _shac_down_widget
    zle -A send-break _shac_orig_send_break
    zle -N send-break _shac_cancel_menu_widget

    add-zsh-hook -D preexec _shac_capture_preexec 2>/dev/null
    add-zsh-hook -D precmd _shac_record_precmd 2>/dev/null
    add-zsh-hook preexec _shac_capture_preexec
    add-zsh-hook precmd _shac_record_precmd
  fi
fi
