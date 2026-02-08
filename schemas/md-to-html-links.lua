function Link(el)
  el.target = el.target:gsub("%.md$", ".html"):gsub("%.md#", ".html#")
  return el
end
