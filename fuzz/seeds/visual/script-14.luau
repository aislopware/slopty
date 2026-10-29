local s = ui.state { tab = "a", on = false, n = 1 }
        return function()
          return ui.column {
            ui.tabs { options = { "a", "b" }, value = s.tab, on_change = function(v) s.tab = v end },
            ui.toggle { label = "Log scale", value = s.on, on_change = function(v) s.on = v end },
            ui.slider { label = "Window", value = s.n, min = 1, max = 10, on_change = function(v) s.n = v end },
            ui.canvas { height = 100, animate = true, draw = function(g, w, h, t) g:line(0, 0, w * t, h) end },
          }
        end
