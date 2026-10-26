/* Recover from synchronous integer, x87 and SSE faults through rt_sigreturn. */
static long call(long n,long a,long b,long c,long d){register long r10 __asm__("r10")=d;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");return r;}
#define CHECK(x) do {if(!(x)){call(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
static volatile unsigned seen,expected_code,expected_trap;
static unsigned long expected_pc,resume_pc;
static unsigned char alternate[32768];
__attribute__((naked)) static void restore(void){__asm__ volatile("mov $15,%rax; syscall");}
static void handler(int signal,void *info,void *context){
 unsigned char *uc=context,*si=info;
 unsigned long *rip=(void *)(uc+168),*trap=(void *)(uc+200);
 unsigned char *fp=(void *)*(unsigned long *)(uc+224);
 unsigned long sp;__asm__ volatile("mov %%rsp,%0":"=r"(sp));
 CHECK(signal==8 && *(int *)(si+8)==(int)expected_code);
 CHECK(*(unsigned long *)(si+16)==expected_pc && *rip==expected_pc && *trap==expected_trap);
 CHECK(sp>(unsigned long)alternate && sp<(unsigned long)alternate+sizeof alternate);
 unsigned short cw;unsigned mxcsr;__asm__ volatile("fnstcw %0; stmxcsr %1":"=m"(cw),"=m"(mxcsr));
 CHECK(cw==0x37f && mxcsr==0x1f80);
 CHECK(fp!=0);
 if(expected_trap==16){CHECK((*(unsigned short *)(fp+2)&0x8080)==0x8080);*(unsigned short *)fp|=63;*(unsigned short *)(fp+2)&=~0x80ff;}
 if(expected_trap==19){CHECK((*(unsigned *)(fp+24)&4)!=0);*(unsigned *)(fp+24)=0x1f80;CHECK(*(unsigned long *)(fp+160)==0x3ff0000000000000ul);}
 *rip=resume_pc;seen++;
}
extern char integer_pc[],integer_done[],x87_pc[],x87_done[],precision_pc[],precision_done[],sse_pc[],sse_done[];
__attribute__((noreturn)) void _start(void){
 struct {unsigned long pointer;unsigned flags,pad;unsigned long size;} stack={(unsigned long)alternate,0,0,sizeof alternate};
 CHECK(call(131,(long)&stack,0,0,0)==0);
 struct {void (*handler)(int,void*,void*);unsigned long flags;void (*restore)(void);unsigned long mask;} action={handler,4|0x4000000|0x8000000,restore,0};
 CHECK(call(13,8,(long)&action,0,8)==0);
 expected_code=1;expected_trap=0;expected_pc=(unsigned long)integer_pc;resume_pc=(unsigned long)integer_done;
 __asm__ volatile("xor %%ecx,%%ecx; xor %%edx,%%edx; mov $1,%%eax; integer_pc: div %%rcx; integer_done:":::"rax","rcx","rdx","cc","memory");
 CHECK(seen==1);
 unsigned short cw=0x37b;
 expected_code=3;expected_trap=16;expected_pc=(unsigned long)x87_pc;resume_pc=(unsigned long)x87_done;
 __asm__ volatile("fninit; fldcw %0; fldz; fld1; fdiv %%st(1),%%st; x87_pc: fwait; x87_done: fninit"::"m"(cw):"st","st(1)","memory");
 CHECK(seen==2);
 cw=0x35f;unsigned three=3;
 expected_code=6;expected_pc=(unsigned long)precision_pc;resume_pc=(unsigned long)precision_done;
 __asm__ volatile("fninit; fldcw %0; fildl %1; fld1; fdiv %%st(1),%%st; precision_pc: fwait; precision_done: fninit"::"m"(cw),"m"(three):"st","st(1)","memory");
 CHECK(seen==3);
 unsigned mxcsr=0x1d80;unsigned long one=0x3ff0000000000000ul;
 expected_code=3;expected_trap=19;expected_pc=(unsigned long)sse_pc;resume_pc=(unsigned long)sse_done;
 __asm__ volatile("ldmxcsr %0; movsd %1,%%xmm0; pxor %%xmm1,%%xmm1; sse_pc: divsd %%xmm1,%%xmm0; sse_done:"::"m"(mxcsr),"m"(one):"xmm0","xmm1","memory");
 CHECK(seen==4);

 for(unsigned mode=0;mode<3;mode++){
  long child=call(57,0,0,0,0);CHECK(child>=0);
  if(child==0){
   if(mode<2){action.handler=(void *)((unsigned long)mode);CHECK(call(13,8,(long)&action,0,8)==0);}
   else {unsigned long blocked=1ul<<7;CHECK(call(14,0,(long)&blocked,0,8)==0);}
   __asm__ volatile("xor %%ecx,%%ecx; xor %%edx,%%edx; mov $1,%%eax; div %%rcx":::"rax","rcx","rdx","cc","memory");
   call(231,99,0,0,0);__builtin_unreachable();
  }
  int status=-1;CHECK(call(61,child,(long)&status,0,0)==child && (status&127)==8);
 }
 call(231,0,0,0,0);__builtin_unreachable();
}
