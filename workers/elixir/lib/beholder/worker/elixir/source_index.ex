defmodule Beholder.Worker.Elixir.SourceIndex do
  @moduledoc false

  alias Beholder.Worker.Elixir.Snapshot.Repository

  alias Beholder.V1.{
    CallableClauseContext,
    ConditionArmContext,
    EvidenceContext,
    PatternArmContext,
    SourceExcerpt,
    SourcePosition,
    SourceRange
  }

  @definitions [:def, :defp, :defdelegate, :defmacro, :defmacrop]
  @non_calls @definitions ++ [:defmodule, :case, :cond, :fn, :quote, :__block__]

  def build(repository) do
    Repository.source_inputs(repository)
    |> Enum.filter(&(Path.extname(&1.path) in [".ex", ".exs"]))
    |> Enum.reduce(%{calls: %{}}, fn input, index ->
      with {:ok, quoted} <-
             Code.string_to_quoted(input.content,
               columns: true,
               token_metadata: true,
               literal_encoder: &encode_literal/2
             ) do
        state = %{
          path: normalize_path(input.path),
          source: input.content,
          lines: String.split(input.content, "\n", trim: false),
          module: nil,
          contexts: []
        }

        walk(quoted, state, index)
      else
        _invalid_source -> index
      end
    end)
  end

  def occurrence(index, path, %{line: line, column: column} = event)
      when is_integer(line) and is_integer(column) and not event.from_macro do
    index.calls
    |> Map.get({path, line, column}, [])
    |> List.first()
  end

  def occurrence(_index, _path, _event), do: nil

  defp walk({:defmodule, _meta, [name, body]}, state, index) do
    module = module_name(name)

    module =
      if state.module && module && !String.contains?(module, "."),
        do: "#{state.module}.#{module}",
        else: module

    walk(keyword_value(body, :do), %{state | module: module}, index)
  end

  defp walk({kind, meta, [head, body]}, %{module: module} = state, index)
       when kind in @definitions and not is_nil(module) do
    {signature, guard} = split_guard(head)

    with {_name, _arities} <- callable(signature),
         definition_range when not is_nil(definition_range) <- metadata_range(meta, state),
         signature_excerpt when not is_nil(signature_excerpt) <- excerpt(head, state) do
      declaration = %CallableClauseContext{
        role: :CALLABLE_CLAUSE_ROLE_DECLARATION,
        signature: signature_excerpt,
        guard: excerpt(guard, state),
        definition_range: definition_range
      }

      enclosing = %EvidenceContext{
        context: {:callable_clause, %{declaration | role: :CALLABLE_CLAUSE_ROLE_ENCLOSING}}
      }

      state = %{state | contexts: [enclosing]}
      index = walk_defaults(signature, state, index)
      index = walk(guard, state, index)
      walk(keyword_value(body, :do), state, index)
    else
      _invalid_definition -> index
    end
  end

  defp walk({:case, _meta, [selector, body]}, state, index) do
    index = walk(selector, state, index)
    selector = excerpt(selector, state)

    body
    |> keyword_value(:do)
    |> clauses()
    |> Enum.reduce(index, &walk_pattern_clause(&1, selector, state, &2))
  end

  defp walk({:cond, _meta, [body]}, state, index) do
    body
    |> keyword_value(:do)
    |> clauses()
    |> Enum.reduce(index, &walk_condition_clause(&1, state, &2))
  end

  defp walk({:fn, _meta, clauses}, state, index) do
    Enum.reduce(clauses, index, &walk_anonymous_clause(&1, state, &2))
  end

  defp walk({name, meta, arguments} = call, state, index)
       when is_atom(name) and is_list(meta) and is_list(arguments) do
    index = if name in @non_calls, do: index, else: record_call(call, meta, state, index)
    Enum.reduce(arguments, index, &walk(&1, state, &2))
  end

  defp walk({{:., _dot_meta, [receiver, name]}, meta, arguments} = call, state, index)
       when is_atom(name) and is_list(meta) and is_list(arguments) do
    index = record_call(call, meta, state, index)
    index = walk(receiver, state, index)
    Enum.reduce(arguments, index, &walk(&1, state, &2))
  end

  defp walk({left, right}, state, index), do: walk(right, state, walk(left, state, index))

  defp walk(values, state, index) when is_list(values),
    do: Enum.reduce(values, index, &walk(&1, state, &2))

  defp walk(_value, _state, index), do: index

  defp walk_pattern_clause({:->, meta, [patterns, body]}, selector, state, index) do
    {pattern, guard} = patterns |> List.first() |> split_guard()
    index = walk(pattern, state, index)
    index = walk(guard, state, index)

    context = %EvidenceContext{
      context:
        {:pattern_arm,
         %PatternArmContext{
           construct: :PATTERN_CONSTRUCT_CASE,
           selector: selector,
           pattern: clause_excerpt(meta, pattern, :pattern, state),
           guard: clause_excerpt(meta, guard, :guard, state),
           is_default: clause_text(meta, pattern, :pattern, state) == "_",
           arm_range: arm_range(meta, body, state)
         }}
    }

    walk(body, %{state | contexts: state.contexts ++ [context]}, index)
  end

  defp walk_pattern_clause(_clause, _selector, _state, index), do: index

  defp walk_condition_clause({:->, meta, [conditions, body]}, state, index) do
    condition = List.first(conditions)
    index = walk(condition, state, index)

    context = %EvidenceContext{
      context:
        {:condition_arm,
         %ConditionArmContext{
           construct: :CONDITION_CONSTRUCT_COND,
           arm: :CONDITION_ARM_KIND_CLAUSE,
           condition: clause_excerpt(meta, condition, :condition, state),
           arm_range: arm_range(meta, body, state)
         }}
    }

    walk(body, %{state | contexts: state.contexts ++ [context]}, index)
  end

  defp walk_condition_clause(_clause, _state, index), do: index

  defp walk_anonymous_clause({:->, meta, [patterns, body]}, state, index) do
    context = anonymous_clause_context(meta, patterns, body, state)
    state = %{state | contexts: if(context, do: [context], else: [])}
    index = walk(patterns, state, index)
    walk(body, state, index)
  end

  defp walk_anonymous_clause(_clause, _state, index), do: index

  defp record_call(call, meta, state, index) do
    with line when is_integer(line) <- meta[:line],
         column when is_integer(column) <- meta[:column],
         range when not is_nil(range) <- expression_range(call, state) do
      occurrence = %{range: range, contexts: state.contexts}

      update_in(
        index.calls,
        &Map.update(&1, {state.path, line, column}, [occurrence], fn calls ->
          [occurrence | calls]
        end)
      )
    else
      _missing_coordinate -> index
    end
  end

  defp callable({name, _meta, arguments}) when is_atom(name) and is_list(arguments) do
    maximum = length(arguments)
    defaults = Enum.count(arguments, &match?({:\\, _, _}, &1))
    {to_string(name), Enum.to_list((maximum - defaults)..maximum)}
  end

  defp callable({name, _meta, nil}) when is_atom(name), do: {to_string(name), [0]}
  defp callable(_head), do: nil

  defp walk_defaults({_name, _meta, arguments}, state, index) when is_list(arguments) do
    Enum.reduce(arguments, index, fn
      {:\\, _meta, [_parameter, default]}, index -> walk(default, state, index)
      _argument, index -> index
    end)
  end

  defp walk_defaults(_signature, _state, index), do: index

  defp split_guard({:when, _meta, [signature, guard]}), do: {signature, guard}
  defp split_guard(value), do: {value, nil}

  defp clauses({:__block__, _meta, clauses}) when is_list(clauses), do: clauses
  defp clauses(clauses) when is_list(clauses), do: clauses
  defp clauses(nil), do: []
  defp clauses(clause), do: [clause]

  defp keyword_value(values, key) when is_list(values), do: Keyword.get(values, key)
  defp keyword_value(_values, _key), do: nil

  defp module_name({:__aliases__, _meta, names}), do: Enum.join(names, ".")

  defp module_name(atom) when is_atom(atom),
    do: atom |> Atom.to_string() |> String.trim_leading("Elixir.")

  defp module_name(_module), do: nil

  defp metadata_range(meta, state) do
    with start when not is_nil(start) <- metadata_position(meta),
         finish when not is_nil(finish) <- metadata_position(meta[:end_of_expression]) do
      source_range(start, finish, state)
    else
      _missing_metadata -> nil
    end
  end

  defp expression_range(ast, state) do
    with start when not is_nil(start) <- expression_start(ast),
         finish when not is_nil(finish) <- expression_end(ast) do
      source_range(start, finish, state)
    else
      _missing_metadata -> nil
    end
  end

  defp expression_start({{:., _meta, [receiver, _name]}, _call_meta, _arguments}),
    do: expression_start(receiver)

  defp expression_start({:when, _meta, [signature, _guard]}), do: expression_start(signature)

  defp expression_start({_name, meta, arguments}) when is_list(meta) do
    Enum.reduce(arguments || [], metadata_position(meta), fn argument, start ->
      min_position(start, expression_start(argument))
    end)
  end

  defp expression_start(values) when is_list(values) do
    Enum.reduce(values, nil, &min_position(&2, expression_start(&1)))
  end

  defp expression_start({left, right}),
    do: min_position(expression_start(left), expression_start(right))

  defp expression_start(_ast), do: nil

  defp expression_end({_name, meta, _arguments} = ast) when is_list(meta) do
    cond do
      position = metadata_position(meta[:closing]) -> advance(position, 1)
      position = metadata_position(meta[:end_of_expression]) -> position
      true -> fallback_end(ast)
    end
  end

  defp expression_end(values) when is_list(values) do
    Enum.reduce(values, nil, &max_position(&2, expression_end(&1)))
  end

  defp expression_end({left, right}),
    do: max_position(expression_end(left), expression_end(right))

  defp expression_end(_ast), do: nil

  defp fallback_end({name, meta, arguments}) when is_atom(name) and is_list(meta) do
    Enum.reduce(
      arguments || [],
      advance(metadata_position(meta), String.length(to_string(name))),
      fn argument, finish ->
        max_position(finish, expression_end(argument))
      end
    )
  end

  defp fallback_end({{:., _meta, [_receiver, name]}, meta, arguments}) do
    Enum.reduce(
      arguments || [],
      advance(metadata_position(meta), String.length(to_string(name))),
      fn argument, finish ->
        max_position(finish, expression_end(argument))
      end
    )
  end

  defp fallback_end(_ast), do: nil

  defp excerpt(nil, _state), do: nil

  defp excerpt(ast, state) do
    with range when not is_nil(range) <- expression_range(ast, state),
         text when is_binary(text) <- range_text(range, state) do
      %SourceExcerpt{text: text, range: range}
    else
      _missing_range -> nil
    end
  end

  defp clause_excerpt(meta, ast, part, state) do
    excerpt(ast, state) ||
      case clause_segment(meta, state) do
        {pattern, guard} -> segment_excerpt(if(part == :guard, do: guard, else: pattern), state)
        nil -> nil
      end
  end

  defp clause_text(meta, ast, part, state) do
    case clause_excerpt(meta, ast, part, state) do
      nil -> nil
      excerpt -> excerpt.text
    end
  end

  defp clause_segment(meta, state) do
    with {line, arrow_column} <- metadata_position(meta),
         source_line when is_binary(source_line) <- Enum.at(state.lines, line - 1) do
      before_arrow = codepoint_prefix(source_line, arrow_column - 1)
      leading = String.length(before_arrow) - String.length(String.trim_leading(before_arrow))
      head = String.trim(before_arrow)

      case String.split(head, " when ", parts: 2) do
        [pattern, guard] ->
          pattern_end = leading + String.length(pattern)
          guard_start = pattern_end + String.length(" when ")

          {{{line, leading + 1}, {line, pattern_end + 1}},
           {{line, guard_start + 1}, {line, guard_start + String.length(guard) + 1}}}

        [condition] ->
          finish = leading + String.length(condition)
          {{{line, leading + 1}, {line, finish + 1}}, nil}
      end
    else
      _missing_arrow -> nil
    end
  end

  defp segment_excerpt(nil, _state), do: nil

  defp segment_excerpt({start, finish}, state) do
    range = source_range(start, finish, state)
    %SourceExcerpt{text: range_text(range, state), range: range}
  end

  defp arm_range(meta, body, state) do
    with {line, column} <- metadata_position(meta),
         finish when not is_nil(finish) <- expression_end(body) do
      start = expression_start(body)
      start = if start && elem(start, 0) == line, do: start, else: {line, column + 2}
      source_range(start, finish, state)
    else
      _missing_range -> nil
    end
  end

  defp anonymous_clause_context(meta, patterns, body, state) do
    with signature when not is_nil(signature) <-
           excerpt(patterns, state) || empty_clause_head(meta, state),
         start when not is_nil(start) <- expression_start(patterns) || metadata_position(meta),
         finish when not is_nil(finish) <- expression_end(body) do
      %EvidenceContext{
        context:
          {:callable_clause,
           %CallableClauseContext{
             role: :CALLABLE_CLAUSE_ROLE_ENCLOSING,
             signature: signature,
             definition_range: source_range(start, finish, state)
           }}
      }
    end
  end

  defp empty_clause_head(meta, state) do
    with position when not is_nil(position) <- metadata_position(meta) do
      range = source_range(position, position, state)
      %SourceExcerpt{text: "", range: range}
    end
  end

  defp source_range(start, finish, state) do
    %SourceRange{start: source_position(start, state), end: source_position(finish, state)}
  end

  defp source_position({line, column}, state) do
    prefix = state.lines |> Enum.at(line - 1, "") |> codepoint_prefix(max(column - 1, 0))

    utf16 =
      prefix |> :unicode.characters_to_binary(:utf8, {:utf16, :little}) |> byte_size() |> div(2)

    %SourcePosition{line: line - 1, character: utf16}
  end

  defp range_text(%SourceRange{start: start, end: finish}, state)
       when start.line == finish.line do
    state.lines
    |> Enum.at(start.line, "")
    |> slice_utf16(start.character, finish.character - start.character)
  end

  defp range_text(%SourceRange{start: start, end: finish}, state)
       when start.line < finish.line do
    state.lines
    |> Enum.slice(start.line..finish.line)
    |> Enum.with_index(start.line)
    |> Enum.map(fn
      {line, index} when index == start.line ->
        slice_utf16(line, start.character, byte_size(line))

      {line, index} when index == finish.line ->
        slice_utf16(line, 0, finish.character)

      {line, _index} ->
        line
    end)
    |> Enum.join("\n")
  end

  defp range_text(_range, _state), do: nil

  defp slice_utf16(text, start, length) do
    text
    |> String.to_charlist()
    |> Enum.reduce({[], 0}, fn character, {selected, offset} ->
      width = if character > 0xFFFF, do: 2, else: 1

      selected =
        if offset >= start and offset < start + length, do: [character | selected], else: selected

      {selected, offset + width}
    end)
    |> elem(0)
    |> Enum.reverse()
    |> List.to_string()
  end

  defp metadata_position(meta) when is_list(meta) do
    case {meta[:line], meta[:column]} do
      {line, column} when is_integer(line) and is_integer(column) -> {line, column}
      _missing -> nil
    end
  end

  defp metadata_position(_meta), do: nil

  defp encode_literal(literal, meta) when is_list(literal),
    do: {:ok, {:__block__, meta, [literal]}}

  defp encode_literal(literal, _meta), do: {:ok, literal}
  defp advance(nil, _columns), do: nil
  defp advance({line, column}, columns), do: {line, column + columns}
  defp max_position(nil, position), do: position
  defp max_position(position, nil), do: position
  defp max_position(left, right), do: max(left, right)
  defp min_position(nil, position), do: position
  defp min_position(position, nil), do: position
  defp min_position(left, right), do: min(left, right)

  defp codepoint_prefix(text, length),
    do: text |> String.to_charlist() |> Enum.take(length) |> List.to_string()

  defp normalize_path(path), do: String.replace(path, "\\", "/")
end
