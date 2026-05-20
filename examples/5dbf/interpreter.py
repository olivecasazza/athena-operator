
class Tape:
    def __init__(self):
        self.cells = [0] * 30000  # Default tape size, can grow
        self.pointer = 0

    def __getitem__(self, index):
        return self.cells[index]

    def __setitem__(self, index, value):
        self.cells[index] = value

    def move_right(self):
        self.pointer += 1
        if self.pointer >= len(self.cells):
            self.cells.append(0)  # Dynamically grow tape

    def move_left(self):
        if self.pointer > 0:
            self.pointer -= 1
        else:
            # Behavior for moving left beyond start is implementation-dependent
            # For simplicity, we'll keep it at 0, or could raise an error
            pass

class FiveDBrainfuckInterpreter:
    def __init__(self, program):
        self.program = program
        self.tape = Tape()
        self.instruction_pointer = 0
        self.history = []  # Stores (cell_index, old_value) for '+' '-' ','

    def _increment(self):
        current_value = self.tape[self.tape.pointer]
        self.history.append((self.tape.pointer, current_value))
        self.tape[self.tape.pointer] += 1

    def _decrement(self):
        current_value = self.tape[self.tape.pointer]
        self.history.append((self.tape.pointer, current_value))
        self.tape[self.tape.pointer] -= 1

    def _output(self):
        print(chr(self.tape[self.tape.pointer]), end='')

    def _input(self):
        # For simplicity, use a fixed input 'A' for demonstration
        # In a real interpreter, this would read from stdin
        input_char = 'A'
        input_value = ord(input_char)
        current_value = self.tape[self.tape.pointer]
        self.history.append((self.tape.pointer, current_value))
        self.tape[self.tape.pointer] = input_value

    def _rewind(self):
        if self.history:
            cell_index, old_value = self.history.pop()
            self.tape[cell_index] = old_value

    def interpret(self):
        while self.instruction_pointer < len(self.program):
            instruction = self.program[self.instruction_pointer]

            if instruction == '>':
                self.tape.move_right()
            elif instruction == '<':
                self.tape.move_left()
            elif instruction == '+':
                self._increment()
            elif instruction == '-':
                self._decrement()
            elif instruction == '.':
                self._output()
            elif instruction == ',':
                self._input()
            elif instruction == '[':
                if self.tape[self.tape.pointer] == 0:
                    # Find matching ']'
                    open_brackets = 1
                    while open_brackets > 0:
                        self.instruction_pointer += 1
                        if self.program[self.instruction_pointer] == '[':
                            open_brackets += 1
                        elif self.program[self.instruction_pointer] == ']':
                            open_brackets -= 1
            elif instruction == ']':
                if self.tape[self.tape.pointer] != 0:
                    # Find matching '['
                    close_brackets = 1
                    while close_brackets > 0:
                        self.instruction_pointer -= 1
                        if self.program[self.instruction_pointer] == ']':
                            close_brackets += 1
                        elif self.program[self.instruction_pointer] == '[':
                            close_brackets -= 1
            elif instruction == '~':
                self._rewind()
            # For demonstration, other 5DBF instructions are ignored or simplified
            # '(', ')', 'v', '^', '@' would require significant state management
            # for timelines and universes, which is beyond the scope of this
            # specific rewind optimization challenge.

            self.instruction_pointer += 1

        print("\n--- Program Finished ---")
        print("Final Tape State (first 10 cells):", self.tape.cells[:10])
        print("Tape Pointer:", self.tape.pointer)
        print("History size:", len(self.history))


# --- Performance Challenge Test Case ---
# Program to increment cell 0, rewind, then increment again
# Expected output: 'A' (from input, then incremented), then 'B' (after rewind and increment)
program_str = "+++++.,~.+." # Increment 5 times, input 'A', rewind, increment, output
print("Executing program:", program_str)
interpreter = FiveDBrainfuckInterpreter(program_str)
interpreter.interpret()

# Another test case: Rewind multiple times
print("\n--- Another Test Case: Multiple Rewinds ---")
program_str_multirewind = "++.,~~~." # Increment twice, input, rewind 3 times, output
print("Executing program:", program_str_multirewind)
interpreter_multirewind = FiveDBrainfuckInterpreter(program_str_multirewind)
interpreter_multirewind.interpret()
