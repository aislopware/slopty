local s = ui.state { speed = 1 }
        return function()
          return ui.column {
            ui.slider { label = "Speed", value = s.speed, min = 0, max = 4, on_change = function(v) s.speed = v end },
            ui.shader { height = 240, orbit = true, inputs = { speed = s.speed }, code = [[
              void mainImage(out vec4 fragColor, in vec2 fragCoord) {
                vec2 uv = fragCoord / iResolution.xy;
                fragColor = vec4(uv, 0.5 + 0.5 * sin(iTime * speed + iCamera.x), 1.0);
              }
            ]] },
          }
        end
