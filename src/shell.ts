const SEPARATORS = ';&|\n()'

export function commandsOf(line: string): string[][] {
  const commands: string[][] = []
  let words: string[] = []
  let word = ''
  let isWord = false
  let quote = ''
  const endWord = () => {
    if (isWord) words.push(word)
    word = ''
    isWord = false
  }
  const endCommand = () => {
    endWord()
    if (words.length > 0) commands.push(words)
    words = []
  }
  for (let at = 0; at < line.length; at += 1) {
    const character = line[at] as string
    const next = line[at + 1]
    if (quote === "'") {
      if (character === "'") quote = ''
      else word += character
    } else if (quote === '"') {
      if (character === '"') quote = ''
      else if (character === '\\' && next !== undefined && '"\\$`'.includes(next)) {
        word += next
        at += 1
      } else word += character
    } else if (character === "'" || character === '"') {
      quote = character
      isWord = true
    } else if (character === '\\' && next !== undefined) {
      word += next
      isWord = true
      at += 1
    } else if (character === ' ' || character === '\t') {
      endWord()
    } else if (SEPARATORS.includes(character)) {
      endCommand()
    } else {
      word += character
      isWord = true
    }
  }
  endCommand()
  return commands.map((command) => command.slice(Math.max(0, command.findIndex((each) => !/^[A-Za-z_][A-Za-z0-9_]*=/.test(each)))))
}

export function programOf(word: string): string {
  return word.slice(word.lastIndexOf('/') + 1)
}
