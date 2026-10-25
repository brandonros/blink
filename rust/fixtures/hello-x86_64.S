.global _start
.text
_start:
 mov $1, %eax
 mov $1, %edi
 lea message(%rip), %rsi
 mov $6, %edx
 syscall
 xor %edi, %edi
 mov $60, %eax
 syscall
.section .rodata
message: .ascii "hello\n"
