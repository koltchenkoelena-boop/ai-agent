defmodule Accounts do
  @moduledoc """
  Модуль для управления пользовательскими аккаунтами.
  """

  @accounts_table :accounts

  @doc """
  Регистрирует нового пользователя.

  ## Параметры
    - username: имя пользователя (строка)
    - password: пароль (строка)
    - initial_balance: начальный баланс (число, по умолчанию 0)

  ## Возвращает
    - {:ok, user} при успешной регистрации
    - {:error, :user_exists} если пользователь уже существует
  """
  def register(username, password, initial_balance \\ 0) do
    case :ets.lookup(@accounts_table, username) do
      [] ->
        hashed_password = hash_password(password)
        user = %{
          username: username,
          password_hash: hashed_password,
          balance: initial_balance,
          inserted_at: DateTime.utc_now()
        }
        :ets.insert(@accounts_table, {username, user})
        {:ok, user}

      [_existing] ->
        {:error, :user_exists}
    end
  end

  @doc """
  Аутентифицирует пользователя по имени и паролю.

  ## Параметры
    - username: имя пользователя
    - password: пароль

  ## Возвращает
    - {:ok, user} при успешной аутентификации
    - {:error, :invalid_credentials} при неверных данных
  """
  def authenticate(username, password) do
    case :ets.lookup(@accounts_table, username) do
      [{^username, user}] ->
        if verify_password(password, user.password_hash) do
          {:ok, user}
        else
          {:error, :invalid_credentials}
        end

      [] ->
        {:error, :invalid_credentials}
    end
  end

  @doc """
  Получает баланс пользователя.

  ## Параметры
    - username: имя пользователя

  ## Возвращает
    - {:ok, balance} при успешном получении баланса
    - {:error, :user_not_found} если пользователь не найден
  """
  def get_balance(username) do
    case :ets.lookup(@accounts_table, username) do
      [{^username, user}] ->
        {:ok, user.balance}

      [] ->
        {:error, :user_not_found}
    end
  end

  @doc """
  Обновляет баланс пользователя.

  ## Параметры
    - username: имя пользователя
    - new_balance: новый баланс

  ## Возвращает
    - {:ok, user} при успешном обновлении
    - {:error, :user_not_found} если пользователь не найден
  """
  def update_balance(username, new_balance) do
    case :ets.lookup(@accounts_table, username) do
      [{^username, user}] ->
        updated_user = %{user | balance: new_balance}
        :ets.insert(@accounts_table, {username, updated_user})
        {:ok, updated_user}

      [] ->
        {:error, :user_not_found}
    end
  end

  @doc """
  Удаляет аккаунт пользователя.

  ## Параметры
    - username: имя пользователя

  ## Возвращает
    - :ok при успешном удалении
    - {:error, :user_not_found} если пользователь не найден
  """
  def delete(username) do
    case :ets.lookup(@accounts_table, username) do
      [{^username, _user}] ->
        :ets.delete(@accounts_table, username)
        :ok

      [] ->
        {:error, :user_not_found}
    end
  end

  @doc """
  Возвращает список всех пользователей (без паролей).
  """
  def list_users do
    :ets.tab2list(@accounts_table)
    |> Enum.map(fn {_, user} -> Map.drop(user, [:password_hash]) end)
  end

  # Приватные функции для хэширования паролей
  defp hash_password(password) do
    :crypto.hash(:sha256, password) |> Base.encode16()
  end

  defp verify_password(password, hash) do
    hash_password(password) == hash
  end

  # Инициализация ETS-таблицы
  def start_link(_opts \\ []) do
    table_opts = [:named_table, :public, read_concurrency: true, write_concurrency: true]
    :ets.new(@accounts_table, table_opts)
    {:ok, self()}
  end
end