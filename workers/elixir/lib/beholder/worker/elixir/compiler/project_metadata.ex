defmodule Beholder.Worker.Elixir.Compiler.ProjectMetadata do
  @moduledoc false

  def read(path) do
    with {:ok, source} <- File.read(path),
         {:ok, quoted} <- Code.string_to_quoted(source),
         {:ok, body} <- mix_project_body(quoted) do
      {:ok, literal_project_requirement(body), otp_version()}
    else
      {:error, reason} when is_binary(reason) -> {:error, reason}
      {:error, reason} -> {:error, inspect(reason)}
    end
  end

  def emit(["metadata", path]) do
    emit("BEHOLDER_PROJECT_METADATA ", read(path))
  end

  def emit(["paths", manifest]) do
    result =
      with {:ok, encoded} <- File.read(manifest),
           paths when is_list(paths) <- :erlang.binary_to_term(encoded, [:safe]),
           true <- Enum.all?(paths, &is_binary/1) do
        validate_local_paths(paths)
      else
        _invalid -> {:error, manifest, :invalid_manifest}
      end

    emit("BEHOLDER_LOCAL_PATHS ", result)
  rescue
    ArgumentError -> emit("BEHOLDER_LOCAL_PATHS ", {:error, manifest, :invalid_manifest})
  end

  defp mix_project_body(quoted) do
    {_quoted, bodies} =
      Macro.prewalk(quoted, [], fn
        {:defmodule, _metadata, [_name, [do: body]]} = node, bodies ->
          if uses_mix_project?(body), do: {node, [body | bodies]}, else: {node, bodies}

        node, bodies ->
          {node, bodies}
      end)

    case bodies do
      [body] -> {:ok, body}
      [] -> {:ok, nil}
      _ -> {:error, "mix.exs defines multiple modules using Mix.Project"}
    end
  end

  defp uses_mix_project?(body) do
    Enum.reduce_while(top_level(body), [], fn
      {:alias, _metadata, [{:__aliases__, _alias_metadata, [:Mix, :Project]}, options]},
      aliases ->
        {:cont, [mix_project_alias(options) | aliases]}

      {:use, _metadata, [{:__aliases__, _alias_metadata, [:Mix, :Project]} | _arguments]},
      _aliases ->
        {:halt, true}

      {:use, _metadata, [{:__aliases__, _alias_metadata, [name]} | _arguments]}, aliases ->
        if name in aliases, do: {:halt, true}, else: {:cont, aliases}

      _node, aliases ->
        {:cont, aliases}
    end) == true
  end

  defp mix_project_alias(options) do
    case Keyword.get(options, :as) do
      {:__aliases__, _metadata, [name]} -> name
      _ -> :Project
    end
  end

  defp literal_project_requirement(body) do
    case Enum.find(top_level(body), fn
           {:def, _metadata, [{:project, _function_metadata, arguments}, _body]}
           when arguments in [nil, []] ->
             true

           _node ->
             false
         end) do
      {:def, _metadata, [{:project, _function_metadata, _arguments}, body]} ->
        literal_keyword(Keyword.get(body, :do), :elixir)

      nil ->
        :none
    end
  end

  defp literal_keyword({:__block__, _metadata, [quoted]}, key),
    do: literal_keyword(quoted, key)

  defp literal_keyword(quoted, key) when is_list(quoted) do
    case Keyword.fetch(quoted, key) do
      {:ok, value} when is_binary(value) -> {:literal, value}
      {:ok, _value} -> :dynamic
      :error -> :none
    end
  end

  defp literal_keyword(_quoted, _key), do: :none

  defp validate_local_paths(paths) do
    Enum.reduce_while(paths, :ok, fn path, :ok ->
      with {:ok, source} <- File.read(path),
           {:ok, quoted} <- Code.string_to_quoted(source) do
        case absolute_local_path(quoted) do
          nil -> {:cont, :ok}
          absolute -> {:halt, {:error, path, absolute}}
        end
      else
        _invalid -> {:halt, {:error, path, :invalid_syntax}}
      end
    end)
  end

  defp absolute_local_path(quoted) do
    {_quoted, path} =
      Macro.prewalk(quoted, nil, fn
        node, path when not is_nil(path) ->
          {node, path}

        {key, value} = node, nil when key in [:path, :apps_path] and is_binary(value) ->
          {node, absolute_path(value)}

        {:import_config, _metadata, [value]} = node, nil when is_binary(value) ->
          {node, absolute_path(value)}

        node, nil ->
          {node, nil}
      end)

    path
  end

  defp absolute_path(value), do: if(Path.type(value) == :absolute, do: value, else: nil)

  defp emit(prefix, result) do
    encoded = result |> :erlang.term_to_binary() |> Base.encode64()
    IO.puts(prefix <> encoded)
  end

  defp otp_version do
    release = to_string(:erlang.system_info(:otp_release))

    case File.read(Path.join([to_string(:code.root_dir()), "releases", release, "OTP_VERSION"])) do
      {:ok, version} -> String.trim(version)
      {:error, _reason} -> to_string(:erlang.system_info(:version))
    end
  end

  defp top_level({:__block__, _metadata, expressions}), do: expressions
  defp top_level(expression), do: [expression]
end
