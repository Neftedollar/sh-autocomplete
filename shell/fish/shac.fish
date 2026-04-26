if set -q SHAC_DISABLE
  # User opted out via SHAC_DISABLE; do nothing.
else if status is-interactive
  if not set -q _SHAC_FISH_LOADED
    set -g _SHAC_FISH_LOADED 1

    set -g _shac_last_request_id ""
    set -g _shac_last_accepted_item_key ""

    # Ctrl+F: accepts the top shac suggestion inline, like zsh's ghost-text accept.
    function __shac_accept_suggestion
      set -l line (commandline)
      if test -z "$line"
        commandline -f forward-char
        return
      end
      set -l cursor (commandline --cursor)
      set -l tty_value (tty 2>/dev/null; or echo "")
      set -l response (TTY=$tty_value shac complete \
        --shell fish \
        --line $line \
        --cursor $cursor \
        --cwd $PWD \
        --format shell-tsv-v2 \
        2>/dev/null | head -n2)
      set -l insert_text ""
      set -l item_key ""
      set -l request_id ""
      for item_line in $response
        set -l parts (string split \t -- $item_line)
        if test "$parts[1]" = "__shac_request_id"; and test (count $parts) -ge 2
          set request_id $parts[2]
        else if test -n "$item_line"; and test (count $parts) -ge 2
          set item_key $parts[1]
          set insert_text $parts[2]
        end
      end
      # Skip full-line replacements (history items containing spaces): commandline -t
      # replaces the current token, so injecting a multi-word value would duplicate
      # words already on the line. Token-level completions (single word) are fine.
      if test -n "$insert_text"; and not string match -q '* *' -- $insert_text
        commandline -t -- $insert_text
        commandline -f end-of-line
        set -g _shac_last_request_id $request_id
        set -g _shac_last_accepted_item_key $item_key
      else
        commandline -f forward-char
      end
    end

    # Clears any accept-state that wasn't consumed by a preexec record (e.g. user
    # accepted with ^F then aborted with ^C). Prevents stale request_id/item_key
    # from being attributed to the next manually-typed command.
    function __shac_reset_accept_state --on-event fish_prompt
      set -g _shac_last_request_id ""
      set -g _shac_last_accepted_item_key ""
    end

    function __shac_record --on-event fish_preexec
      set -l cmd $argv[1]
      switch $cmd
        case 'shac *' '_shac_*' ''
          return
      end
      set -l -a record_args \
        --shell fish \
        --cwd $PWD \
        --command $cmd \
        --trust interactive \
        --provenance typed_manual \
        --origin fish_preexec \
        --tty-present
      test -n "$_shac_last_request_id"; \
        and set -a record_args --accepted-request-id $_shac_last_request_id
      test -n "$_shac_last_accepted_item_key"; \
        and set -a record_args --accepted-item-key $_shac_last_accepted_item_key
      shac record-command $record_args >/dev/null 2>&1
      set -g _shac_last_request_id ""
      set -g _shac_last_accepted_item_key ""
    end

    # v0.2.0 fish integration scope:
    #   - Ctrl+F: accept the top shac suggestion (works for any commandline state)
    #   - fish_preexec hook: record commands into the shac DB
    # Tab still uses fish's native completion. fish has no documented "match-all"
    # form of `complete`, so a global tab override is intentionally deferred to
    # a later version that registers per-command completions on demand.
    bind \cf __shac_accept_suggestion
    bind -M insert \cf __shac_accept_suggestion 2>/dev/null; or true
  end
end
