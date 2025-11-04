import React from 'react'
import { marked } from 'marked'
import hljs from 'highlight.js'
import 'highlight.js/styles/github-dark.css'
import ToolCall from './ToolCall'

function ChatView({ messages, toolCalls }) {
  const renderMessage = (message) => {
    const html = marked(message.content)
    
    return (
      <div
        key={message.id}
        className={`p-4 rounded-lg mb-4 ${
          message.agent === 'coach'
            ? 'bg-blue-50 dark:bg-blue-900/20 border-l-4 border-blue-500'
            : message.agent === 'player'
            ? 'bg-gray-50 dark:bg-gray-800 border-l-4 border-gray-500'
            : 'bg-white dark:bg-gray-700'
        }`}
      >
        <div className="flex items-center gap-2 mb-2">
          <span className="text-xs font-semibold text-gray-600 dark:text-gray-400">
            {message.agent.toUpperCase()}
          </span>
          <span className="text-xs text-gray-500 dark:text-gray-500">
            {new Date(message.timestamp).toLocaleTimeString()}
          </span>
        </div>
        <div
          className="markdown prose dark:prose-invert max-w-none"
          dangerouslySetInnerHTML={{ __html: html }}
        />
      </div>
    )
  }

  React.useEffect(() => {
    // Highlight code blocks after render
    document.querySelectorAll('pre code').forEach((block) => {
      hljs.highlightElement(block)
    })
  }, [messages])

  if (messages.length === 0 && toolCalls.length === 0) {
    return (
      <div className="text-center text-gray-600 dark:text-gray-400 py-8">
        No messages yet
      </div>
    )
  }

  return (
    <div className="space-y-4 max-h-[600px] overflow-y-auto">
      {messages.map(renderMessage)}
      
      {toolCalls.length > 0 && (
        <div className="mt-6">
          <h4 className="text-lg font-semibold text-gray-900 dark:text-white mb-4">
            Tool Calls
          </h4>
          {toolCalls.map((toolCall) => (
            <ToolCall key={toolCall.id} toolCall={toolCall} />
          ))}
        </div>
      )}
    </div>
  )
}

export default ChatView
