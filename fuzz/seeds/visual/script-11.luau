return ui.canvas { height = 120, draw = function(g, w, h)
          g:line(0, h, w, 0, { color = "border" })
          g:rect(10, 10, 40, 20, { fill = "chart2", radius = 4 })
          g:circle(w / 2, h / 2, 8, { color = ui.color("accent", 0.5) })
          g:arc(60, 60, 30, 0, math.pi, { fill = "chart3", inner = 20 })
          g:text(w, 0, "label", { align = "end", color = "muted", size = "xs" })
        end }
