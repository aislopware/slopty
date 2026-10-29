return function()
          return ui.shader { height = 200, code = [[
void mainImage(out vec4 fragColor, in vec2 fragCoord) {
  fragColor = vec4(missing, 1.0);
}
          ]] }
        end
