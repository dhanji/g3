import React, { useState } from 'react'

function ToolCall({ toolCall }) {
  const [expanded, setExpanded] = useState(false)

  return (
    <div className="bg-gray-100 dark:bg-gray-800 rounded-lg p-4 mb-3">
      <div
        className="flex justify-between items-center cursor-pointer"
        onClick={() => setExpanded(!expanded)}
      >
        <div className="flex items-center gap-3">
          <span className="font-mono text-sm font-semibold text-gray-900 dark:text-white">
            {toolCall.tool_name}
          </span>
          {toolCall.success ? (
            <span className="hero-badge hero-badge-success">SUCCESS</span>
          ) : (
            <span className="hero-badge hero-badge-error">FAILED</span>
          )}
          {toolCall.execution_time_ms && (
            <span className="text-xs text-gray-600 dark:text-gray-400">
              {toolCall.execution_time_ms}ms
            </span>
          )}
        </div>
        <button className="text-gray-600 dark:text-gray-400">
          {expanded ? '▼' : '▶'}
        </button>
      </div>

      {expanded && (
        <div className="mt-4 space-y-3">
          <div>
            <div className="text-xs font-semibold text-gray-600 dark:text-gray-400 mb-1">
              Parameters
            </div>
            <pre className="text-xs bg-white dark:bg-gray-900 p-2 rounded overflow-x-auto">
              {JSON.stringify(toolCall.parameters, null, 2)}
            </pre>
          </div>

          {toolCall.result && (
            <div>
              <div className="text-xs font-semibold text-gray-600 dark:text-gray-400 mb-1">
                Result
              </div>
              <pre className="text-xs bg-white dark:bg-gray-900 p-2 rounded overflow-x-auto">
                {JSON.stringify(toolCall.result, null, 2)}
              </pre>
            </div>
          )}

          {toolCall.error && (
            <div>
              <div className="text-xs font-semibold text-red-600 dark:text-red-400 mb-1">
                Error
              </div>
              <pre className="text-xs bg-red-50 dark:bg-red-900/20 p-2 rounded text-red-800 dark:text-red-200">
                {toolCall.error}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  )
}

export default ToolCall
