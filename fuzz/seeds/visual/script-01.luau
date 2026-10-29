local s = ui.state { tab = "p95" }
        return function()
          return ui.column { gap = 12,
            ui.text { "Latency ", ui.span { s.tab, weight = 600, color = "accent" }, size = "lg" },
            ui.tabs { options = { "p95", "p99" }, value = s.tab, on_change = function(v) s.tab = v end },
          }
        end
