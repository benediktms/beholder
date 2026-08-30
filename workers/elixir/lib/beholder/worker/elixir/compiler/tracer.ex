defmodule Beholder.Worker.Elixir.Compiler.Tracer do
  @moduledoc false

  alias Beholder.Worker.Elixir.Compiler.Collector

  @spec trace(term(), Macro.Env.t()) :: :ok
  def trace(:start, env), do: record(:source_start, [], %{}, env)
  def trace(:stop, _env), do: :ok

  def trace({:alias, meta, target, as, _options}, env),
    do: record(:alias, meta, %{target: module_name(target), as: module_name(as)}, env)

  def trace({:alias_expansion, meta, _as, target}, env),
    do: record(:alias_expansion, meta, %{target: module_name(target)}, env)

  def trace({:alias_reference, meta, target}, env),
    do: record(:alias_reference, meta, %{target: module_name(target)}, env)

  def trace({:import, meta, target, _options}, env),
    do: record(:import, meta, %{target: module_name(target)}, env)

  def trace({:require, meta, target, _options}, env),
    do: record(:require, meta, %{target: module_name(target)}, env)

  def trace({:struct_expansion, meta, target, keys}, env),
    do:
      record(
        :struct_expansion,
        meta,
        %{target: module_name(target), keys: Enum.map(keys, &to_string/1)},
        env
      )

  def trace({kind, meta, target, name, arity}, env)
      when kind in [:imported_function, :imported_macro, :remote_function, :remote_macro] do
    record(kind, meta, %{target: module_name(target), name: to_string(name), arity: arity}, env)
  end

  def trace({kind, meta, name, arity}, env) when kind in [:local_function, :local_macro] do
    record(kind, meta, %{name: to_string(name), arity: arity}, env)
  end

  def trace({:on_module, _bytecode, _metadata}, env) do
    definitions =
      if env.module && Module.open?(env.module) do
        Enum.map(Module.definitions_in(env.module), fn {name, arity} ->
          {to_string(name), arity}
        end)
      else
        []
      end

    record(:module, [], %{target: module_name(env.module), definitions: definitions}, env)
  end

  def trace(_event, _env), do: :ok

  defp record(kind, meta, event, env) do
    Collector.record(
      Map.merge(event, %{
        kind: kind,
        file: to_string(env.file),
        line: Keyword.get(meta, :line, env.line),
        column: Keyword.get(meta, :column),
        caller_module: module_name(env.module),
        caller_function: normalize_function(env.function),
        from_macro: Keyword.has_key?(meta, :from_macro)
      })
    )

    :ok
  end

  defp module_name(nil), do: nil

  defp module_name(module) when is_atom(module) do
    case Atom.to_string(module) do
      "Elixir." <> name -> name
      name -> ":" <> name
    end
  end

  defp normalize_function({name, arity}), do: {to_string(name), arity}
  defp normalize_function(nil), do: nil
end
