local s = ui.state { tab = "a" }
        return function()
          local body = s.tab == "a" and ui.text "fine" or ui.text { nil .. "boom" }
          return ui.column {
            ui.tabs { options = { "a", "b" }, value = s.tab, on_change = function(v) s.tab = v end },
            body,
          }
        end
