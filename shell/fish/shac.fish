if set -q SHAC_DISABLE
  # User opted out via SHAC_DISABLE; do nothing.
else if status is-interactive
  if not set -q _SHAC_FISH_LOADED
    set -g _SHAC_FISH_LOADED 1

    set -g _shac_last_request_id ""
    set -g _shac_last_accepted_item_key ""

    # Reverse shell-tsv-v3 field encoding (\\ -> \, \t -> tab, \n -> newline,
    # \r -> CR); any other backslash+char pair passes through literally so it
    # round-trips arbitrary already-escaped-looking input. Mirrors
    # src/wire.rs::decode_field byte-for-byte.
    function __shac_decode
      set -l s $argv[1]
      if test -z "$s"
        printf ''
        return 0
      end
      set -l tab \t
      set -l nl \n
      set -l cr \r
      set -l chars (string split '' -- $s)
      set -l n (count $chars)
      set -l out ""
      set -l i 1
      while test $i -le $n
        set -l c $chars[$i]
        if test "$c" = "\\"
          set i (math $i + 1)
          if test $i -gt $n
            set out "$out\\"
          else
            set -l nc $chars[$i]
            if test "$nc" = "\\"
              set out "$out\\"
            else if test "$nc" = t
              set out "$out$tab"
            else if test "$nc" = n
              set out "$out$nl"
            else if test "$nc" = r
              set out "$out$cr"
            else
              set out "$out\\$nc"
            end
          end
        else
          set out "$out$c"
        end
        set i (math $i + 1)
      end
      printf '%s' $out
    end

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
        --format shell-tsv-v3 \
        2>/dev/null | head -n2)
      set -l insert_text ""
      set -l item_key ""
      set -l request_id ""
      set -l found_item 0
      for item_line in $response
        set -l parts (string split \t -- $item_line)
        if test "$parts[1]" = "__shac_request_id"; and test (count $parts) -ge 2
          # `| string collect` keeps a decoded field that contains a literal
          # newline as ONE value: an uncollected `(cmd)` capture splits on
          # every newline in cmd's output into a separate list element, so a
          # decoded field would fracture and later expand as multiple
          # positional args instead of the single string it decoded to.
          set request_id (__shac_decode $parts[2] | string collect)
          # "0" is the daemon's no-traceable-request sentinel (zero-candidate
          # response, kept non-empty for wire alignment — F2). Treat as none so
          # a later accept never sends --accepted-request-id 0 (codex/#40).
          test "$request_id" = "0"; and set request_id ""
        else if string match -q '__shac_*' -- $parts[1]
          # Skip any other sentinel rows (e.g. __shac_tip) — inline accept only
          # consumes a real candidate row, mirrors zsh shac.zsh _shac_fetch_inline.
          continue
        else if test $found_item -eq 0; and test -n "$item_line"; and test (count $parts) -ge 2
          set item_key (__shac_decode $parts[1] | string collect)
          set insert_text (__shac_decode $parts[2] | string collect)
          set found_item 1
        end
      end
      # insert_text is already a final shell literal (daemon-quoted, e.g. an
      # embedded space arrives backslash-escaped): insert it verbatim, no
      # further quoting or space guard needed. `-t` replace targets fish's
      # own notion of "the current token", which already spans a leading
      # unterminated quote the user typed (e.g. `cd "My D` -> token is
      # `"My D`, not `My D`) — so this one call also satisfies the F5
      # contract that the WHOLE active token, quote char included, gets
      # replaced by the (now self-contained, open-and-close) quoted literal
      # quote_token produces for that case. Do not switch this to `-i`
      # insert or any suffix-append scheme: that would leave the user's
      # already-typed opening quote on the line and double it up.
      if test -n "$insert_text"
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
