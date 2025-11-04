import React from 'react'

function GitStatus({ status }) {
  return (
    <div>
      <h4 className="font-semibold text-gray-900 dark:text-white mb-2">Git Status</h4>
      <div className="space-y-2">
        <div className="text-sm">
          <span className="text-gray-600 dark:text-gray-400">Branch:</span>
          <span className="ml-2 font-mono text-gray-900 dark:text-white">{status.branch}</span>
        </div>
        <div className="text-sm">
          <span className="text-gray-600 dark:text-gray-400">Uncommitted changes:</span>
          <span className="ml-2 font-semibold text-gray-900 dark:text-white">
            {status.uncommitted_changes}
          </span>
        </div>

        {status.modified_files.length > 0 && (
          <div>
            <div className="text-xs font-semibold text-yellow-600 dark:text-yellow-400 mb-1">
              Modified ({status.modified_files.length})
            </div>
            <ul className="text-xs text-gray-700 dark:text-gray-300 space-y-1">
              {status.modified_files.map((file, i) => (
                <li key={i} className="font-mono">• {file}</li>
              ))}
            </ul>
          </div>
        )}

        {status.added_files.length > 0 && (
          <div>
            <div className="text-xs font-semibold text-green-600 dark:text-green-400 mb-1">
              Added ({status.added_files.length})
            </div>
            <ul className="text-xs text-gray-700 dark:text-gray-300 space-y-1">
              {status.added_files.map((file, i) => (
                <li key={i} className="font-mono">• {file}</li>
              ))}
            </ul>
          </div>
        )}

        {status.deleted_files.length > 0 && (
          <div>
            <div className="text-xs font-semibold text-red-600 dark:text-red-400 mb-1">
              Deleted ({status.deleted_files.length})
            </div>
            <ul className="text-xs text-gray-700 dark:text-gray-300 space-y-1">
              {status.deleted_files.map((file, i) => (
                <li key={i} className="font-mono">• {file}</li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  )
}

export default GitStatus
